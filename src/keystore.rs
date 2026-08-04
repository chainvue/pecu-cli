//! Private keys on disk, encrypted.
//!
//! One file per key, under `<config>/keys/<label>.json`. Each is a self-
//! describing envelope: the KDF and its parameters travel with the ciphertext,
//! so raising the cost for new keys never strands the old ones.
//!
//! # What is protected, and from what
//!
//! Argon2id derives a 32-byte key from the passphrase; ChaCha20-Poly1305 seals
//! the 32 private key bytes under it. The envelope's metadata — version, label,
//! address, compression flag — is authenticated as associated data, so someone
//! who edits the address in the file gets a decryption failure rather than a key
//! that silently belongs to a different address than it claims.
//!
//! This defends a stolen file against an offline guess. It does not defend a
//! running process: once unlocked, the key is in memory. It is held in
//! `Zeroizing` wrappers throughout and wiped on drop, which limits the window
//! but does not close it.
//!
//! # Where the entropy comes from
//!
//! `verus-keys` deliberately offers no `PrivateKey::generate`: where the bytes
//! come from is the most security-critical decision a wallet makes, and a
//! library that picks quietly moves it somewhere nobody reviews. So it is here,
//! in the open, and it is [`getrandom`] — the OS CSPRNG on every platform.

use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use verus_sdk::verus_keys::{KeyError, PrivateKey};
use zeroize::Zeroizing;

use crate::config::Paths;

/// Bumped when the envelope's shape changes in a way older readers cannot cope
/// with. Reading refuses anything it does not know.
pub const ENVELOPE_VERSION: u32 = 1;

/// Argon2id cost for a new key, following the OWASP interactive recommendation:
/// 19 MiB, two passes, one lane. Stored per key, so this can be raised later
/// without invalidating anything already written.
const MEMORY_KIB: u32 = 19 * 1024;
const ITERATIONS: u32 = 2;
const PARALLELISM: u32 = 1;

const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
const DERIVED_BYTES: usize = 32;

/// Set to skip the interactive prompt. For scripts and tests; a passphrase in
/// the environment is readable by anything that can read `/proc` or run `ps -E`.
pub const PASSPHRASE_ENV: &str = "PECU_PASSPHRASE";

#[derive(Debug, Error, Diagnostic)]
pub enum KeystoreError {
    #[error("`{label}` is not a usable key name")]
    #[diagnostic(
        code(pecu::bad_label),
        help("use lowercase letters, digits, `-` and `_`; start with a letter or digit, at most 64 characters")
    )]
    BadLabel { label: String },

    #[error("there is already a key called `{label}`")]
    #[diagnostic(
        code(pecu::key_exists),
        help("pick another name, or delete {} yourself", path.display())
    )]
    Exists { label: String, path: PathBuf },

    #[error("no key called `{label}`")]
    #[diagnostic(code(pecu::no_such_key), help("{known}"))]
    NotFound { label: String, known: String },

    #[error("cannot {action} {}", path.display())]
    #[diagnostic(code(pecu::keystore_io))]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{} is not a key file this version understands", path.display())]
    #[diagnostic(code(pecu::keystore_corrupt), help("{detail}"))]
    Corrupt { path: PathBuf, detail: String },

    #[error("wrong passphrase for `{label}`")]
    #[diagnostic(
        code(pecu::wrong_passphrase),
        help("the file is intact — this is the passphrase, or a key file edited by hand")
    )]
    WrongPassphrase { label: String },

    #[error("a passphrase is required")]
    #[diagnostic(
        code(pecu::empty_passphrase),
        help("an unencrypted key file is a plaintext private key; if you want that, use VERUS_WIF instead and keep it out of the keystore")
    )]
    EmptyPassphrase,

    #[error("the two passphrases did not match")]
    #[diagnostic(code(pecu::passphrase_mismatch))]
    PassphraseMismatch,

    #[error("cannot read a passphrase")]
    #[diagnostic(
        code(pecu::no_tty),
        help("there is no terminal to prompt on — set {PASSPHRASE_ENV} instead")
    )]
    NoPrompt {
        #[source]
        source: std::io::Error,
    },

    #[error("that is not a valid private key")]
    #[diagnostic(code(pecu::bad_key))]
    Key {
        #[source]
        source: KeyError,
    },

    #[error("the operating system would not give us random bytes")]
    #[diagnostic(
        code(pecu::no_entropy),
        help("this is the OS CSPRNG failing; do not work around it — a key from a weak source protects nothing")
    )]
    NoEntropy,
}

/// Key derivation, as recorded in the envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kdf {
    /// Only `argon2id` today. Present so a future algorithm can be added
    /// without every existing file becoming ambiguous.
    pub algorithm: String,
    pub salt: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cipher {
    pub algorithm: String,
    pub nonce: String,
}

/// One key on disk. Everything here except `ciphertext` is public information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u32,
    pub label: String,
    /// The transparent address this key controls, so `pecu key list` needs no
    /// passphrase to be useful.
    pub address: String,
    pub compressed: bool,
    /// Unix seconds. Stored as a number rather than a formatted date to avoid a
    /// calendar dependency for something only ever shown as an age.
    pub created: u64,
    pub kdf: Kdf,
    pub cipher: Cipher,
    pub ciphertext: String,
}

impl Envelope {
    /// Metadata bound into the AEAD, so editing any of it invalidates the
    /// ciphertext instead of quietly changing what the file claims.
    fn associated_data(&self) -> String {
        format!(
            "pecu-key-v{}\n{}\n{}\n{}",
            self.version, self.label, self.address, self.compressed
        )
    }

    /// Decrypt, and check that what came out really does control the address
    /// this file advertises.
    pub fn unlock(&self, passphrase: &str) -> Result<PrivateKey, KeystoreError> {
        let path = PathBuf::from(format!("{}.json", self.label));
        let corrupt = |detail: &str| KeystoreError::Corrupt {
            path: path.clone(),
            detail: detail.to_string(),
        };

        if self.version != ENVELOPE_VERSION {
            return Err(corrupt(&format!(
                "envelope version {} — this build writes and reads version {ENVELOPE_VERSION}",
                self.version
            )));
        }
        if self.kdf.algorithm != "argon2id" {
            return Err(corrupt(&format!("unknown kdf `{}`", self.kdf.algorithm)));
        }
        if self.cipher.algorithm != "chacha20poly1305" {
            return Err(corrupt(&format!(
                "unknown cipher `{}`",
                self.cipher.algorithm
            )));
        }

        let salt = hex::decode(&self.kdf.salt).map_err(|_| corrupt("salt is not hex"))?;
        let nonce = hex::decode(&self.cipher.nonce).map_err(|_| corrupt("nonce is not hex"))?;
        let ciphertext =
            hex::decode(&self.ciphertext).map_err(|_| corrupt("ciphertext is not hex"))?;
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| corrupt("nonce is the wrong length"))?;

        let derived = derive(
            passphrase,
            &salt,
            self.kdf.memory_kib,
            self.kdf.iterations,
            self.kdf.parallelism,
        )
        .map_err(|detail| corrupt(&detail))?;

        let aad = self.associated_data();
        let plaintext = cipher(&derived)
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            // The AEAD cannot tell a wrong passphrase from tampered metadata,
            // and neither can we. The passphrase is overwhelmingly the likelier
            // of the two, so that is what the message says.
            .map_err(|_| KeystoreError::WrongPassphrase {
                label: self.label.clone(),
            })?;
        let plaintext = Zeroizing::new(plaintext);

        let bytes: [u8; 32] = plaintext
            .as_slice()
            .try_into()
            .map_err(|_| corrupt("decrypted material is not 32 bytes"))?;
        let key = PrivateKey::from_bytes(&Zeroizing::new(bytes), self.compressed)
            .map_err(|source| KeystoreError::Key { source })?;

        // The address is authenticated, but that only proves nobody edited it —
        // not that whoever wrote the file was consistent. Cheap to check.
        if key.address().to_string() != self.address {
            return Err(corrupt(
                "the decrypted key does not control the address in this file",
            ));
        }
        Ok(key)
    }
}

/// The directory of key files.
pub struct Keystore {
    dir: PathBuf,
}

impl Keystore {
    pub fn new(paths: &Paths) -> Self {
        Self {
            dir: paths.keys_dir(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path_for(&self, label: &str) -> PathBuf {
        self.dir.join(format!("{label}.json"))
    }

    pub fn exists(&self, label: &str) -> bool {
        self.path_for(label).exists()
    }

    /// Every key, oldest first. A missing directory is an empty keystore.
    pub fn list(&self) -> Result<Vec<Envelope>, KeystoreError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(KeystoreError::Io {
                    action: "list",
                    path: self.dir.clone(),
                    source,
                })
            }
        };

        let mut envelopes = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            envelopes.push(self.read(&path)?);
        }
        envelopes.sort_by_key(|envelope| (envelope.created, envelope.label.clone()));
        Ok(envelopes)
    }

    pub fn load(&self, label: &str) -> Result<Envelope, KeystoreError> {
        check_label(label)?;
        let path = self.path_for(label);
        if !path.exists() {
            return Err(KeystoreError::NotFound {
                label: label.to_string(),
                known: self.suggestion(),
            });
        }
        self.read(&path)
    }

    fn read(&self, path: &Path) -> Result<Envelope, KeystoreError> {
        let text = std::fs::read_to_string(path).map_err(|source| KeystoreError::Io {
            action: "read",
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|error| KeystoreError::Corrupt {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })
    }

    /// What to suggest when a label was not found.
    fn suggestion(&self) -> String {
        match self.list() {
            Ok(keys) if keys.is_empty() => "the keystore is empty — try `pecu key gen`".to_string(),
            Ok(keys) => format!(
                "known keys: {}",
                keys.iter()
                    .map(|envelope| envelope.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Err(_) => "run `pecu key list` to see what is there".to_string(),
        }
    }

    /// Encrypt `key` under `passphrase` and write it as `label`.
    ///
    /// Refuses to overwrite. A keystore that silently replaced a key would
    /// destroy funds on a mistyped label.
    pub fn store(
        &self,
        label: &str,
        key: &PrivateKey,
        passphrase: &str,
    ) -> Result<Envelope, KeystoreError> {
        check_label(label)?;
        if passphrase.is_empty() {
            return Err(KeystoreError::EmptyPassphrase);
        }
        let path = self.path_for(label);
        if path.exists() {
            return Err(KeystoreError::Exists {
                label: label.to_string(),
                path,
            });
        }

        let mut salt = [0u8; SALT_BYTES];
        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut salt).map_err(|_| KeystoreError::NoEntropy)?;
        getrandom::fill(&mut nonce).map_err(|_| KeystoreError::NoEntropy)?;

        let mut envelope = Envelope {
            version: ENVELOPE_VERSION,
            label: label.to_string(),
            address: key.address().to_string(),
            compressed: key.is_compressed(),
            created: now(),
            kdf: Kdf {
                algorithm: "argon2id".into(),
                salt: hex::encode(salt),
                memory_kib: MEMORY_KIB,
                iterations: ITERATIONS,
                parallelism: PARALLELISM,
            },
            cipher: Cipher {
                algorithm: "chacha20poly1305".into(),
                nonce: hex::encode(nonce),
            },
            ciphertext: String::new(),
        };

        let derived =
            derive(passphrase, &salt, MEMORY_KIB, ITERATIONS, PARALLELISM).map_err(|detail| {
                KeystoreError::Corrupt {
                    path: path.clone(),
                    detail,
                }
            })?;
        let aad = envelope.associated_data();
        let secret = key.to_bytes();
        let sealed = cipher(&derived)
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: secret.as_ref(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| KeystoreError::Corrupt {
                path: path.clone(),
                detail: "encryption failed".into(),
            })?;
        envelope.ciphertext = hex::encode(sealed);

        self.write(&path, &envelope)?;
        Ok(envelope)
    }

    fn write(&self, path: &Path, envelope: &Envelope) -> Result<(), KeystoreError> {
        std::fs::create_dir_all(&self.dir).map_err(|source| KeystoreError::Io {
            action: "create",
            path: self.dir.clone(),
            source,
        })?;
        restrict(&self.dir, 0o700)?;

        let json = serde_json::to_string_pretty(envelope).expect("the envelope is plain data");
        std::fs::write(path, format!("{json}\n")).map_err(|source| KeystoreError::Io {
            action: "write",
            path: path.to_path_buf(),
            source,
        })?;
        restrict(path, 0o600)
    }
}

/// Tighten permissions. A no-op off Unix, where the containing profile is the
/// protection.
fn restrict(path: &Path, mode: u32) -> Result<(), KeystoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(
            |source| KeystoreError::Io {
                action: "set permissions on",
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// The AEAD under a derived key. Infallible in practice — the key is a fixed
/// 32-byte array — but `new_from_slice` is the non-deprecated constructor.
fn cipher(derived: &[u8; DERIVED_BYTES]) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new(&Key::from(*derived))
}

fn derive(
    passphrase: &str,
    salt: &[u8],
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
) -> Result<Zeroizing<[u8; DERIVED_BYTES]>, String> {
    let params = Params::new(memory_kib, iterations, parallelism, Some(DERIVED_BYTES))
        .map_err(|error| format!("unusable argon2 parameters: {error}"))?;
    let mut derived = Zeroizing::new([0u8; DERIVED_BYTES]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, derived.as_mut())
        .map_err(|error| format!("key derivation failed: {error}"))?;
    Ok(derived)
}

/// A label becomes a filename, so this is a security check, not a style rule.
pub fn check_label(label: &str) -> Result<(), KeystoreError> {
    let bad = || KeystoreError::BadLabel {
        label: label.to_string(),
    };
    if label.is_empty() || label.len() > 64 {
        return Err(bad());
    }
    if !label
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
    {
        return Err(bad());
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(bad());
    }
    Ok(())
}

/// 32 bytes from the OS CSPRNG.
pub fn entropy() -> Result<Zeroizing<[u8; 32]>, KeystoreError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| KeystoreError::NoEntropy)?;
    Ok(bytes)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Read a secret that is *input* rather than a passphrase: a WIF being
/// imported, a recovery phrase being typed back in.
///
/// Deliberately does **not** consult [`PASSPHRASE_ENV`]. `key import` needs two
/// different secrets in one run — the key, and the passphrase to seal it under —
/// and one environment variable cannot supply both without silently using the
/// same value for each.
///
/// Reads stdin when stdin is not a terminal, so the command can be piped;
/// prompts without echo otherwise. Only the trailing newline is stripped —
/// interior whitespace is significant in a seed phrase, which is hashed
/// verbatim.
pub fn read_secret(prompt: &str) -> Result<Zeroizing<String>, KeystoreError> {
    use std::io::{BufRead, IsTerminal};

    if std::io::stdin().is_terminal() {
        return Ok(Zeroizing::new(
            rpassword::prompt_password(format!("{prompt}: "))
                .map_err(|source| KeystoreError::NoPrompt { source })?,
        ));
    }

    let mut line = Zeroizing::new(String::new());
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|source| KeystoreError::NoPrompt { source })?;
    let trimmed = line.trim_end_matches(['\n', '\r']);
    Ok(Zeroizing::new(trimmed.to_string()))
}

/// Ask for the passphrase a key is encrypted under, or take one from the
/// environment.
///
/// `confirm` asks twice, which is what you want when the answer will be needed
/// again later and there is nothing to check it against.
pub fn passphrase(prompt: &str, confirm: bool) -> Result<Zeroizing<String>, KeystoreError> {
    if let Ok(from_env) = std::env::var(PASSPHRASE_ENV) {
        return Ok(Zeroizing::new(from_env));
    }
    let first = Zeroizing::new(
        rpassword::prompt_password(format!("{prompt}: "))
            .map_err(|source| KeystoreError::NoPrompt { source })?,
    );
    if confirm {
        let again = Zeroizing::new(
            rpassword::prompt_password("confirm: ")
                .map_err(|source| KeystoreError::NoPrompt { source })?,
        );
        if *first != *again {
            return Err(KeystoreError::PassphraseMismatch);
        }
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(store: &Keystore, label: &str, key: &PrivateKey, passphrase: &str) -> Envelope {
        store.store(label, key, passphrase).expect("stored")
    }

    fn key() -> PrivateKey {
        PrivateKey::from_bytes(&[7u8; 32], true).expect("a valid key")
    }

    fn store() -> (tempfile::TempDir, Keystore) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = Keystore {
            dir: dir.path().join("keys"),
        };
        (dir, store)
    }

    #[test]
    fn a_stored_key_round_trips() {
        let (_guard, store) = store();
        let original = key();
        let envelope = stored(&store, "demo", &original, "correct horse");

        let recovered = envelope.unlock("correct horse").expect("unlocked");
        assert_eq!(*recovered.to_wif(), *original.to_wif());
        assert_eq!(envelope.address, original.address().to_string());
    }

    #[test]
    fn the_wrong_passphrase_is_refused() {
        let (_guard, store) = store();
        let envelope = stored(&store, "demo", &key(), "correct horse");
        let error = envelope.unlock("battery staple").unwrap_err();
        assert!(
            matches!(error, KeystoreError::WrongPassphrase { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn no_plaintext_key_material_reaches_the_file() {
        let (_guard, store) = store();
        let secret = key();
        stored(&store, "demo", &secret, "correct horse");

        let written = std::fs::read_to_string(store.path_for("demo")).expect("readable");
        assert!(
            !written.contains(&*secret.to_wif() as &str),
            "the WIF is in the file"
        );
        assert!(
            !written.contains(&hex::encode(*secret.to_bytes())),
            "the raw key bytes are in the file"
        );
        // The public half is meant to be there — that is what makes `key list`
        // work without a passphrase.
        assert!(written.contains(&secret.address().to_string()));
    }

    #[test]
    fn editing_the_address_invalidates_the_file() {
        let (_guard, store) = store();
        let mut envelope = stored(&store, "demo", &key(), "correct horse");
        envelope.address = "RSomeoneElsesAddress".into();

        // Authenticated as associated data, so this fails to decrypt at all
        // rather than decrypting into a mislabelled key.
        let error = envelope.unlock("correct horse").unwrap_err();
        assert!(
            matches!(error, KeystoreError::WrongPassphrase { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn an_unknown_envelope_version_is_refused() {
        let (_guard, store) = store();
        let mut envelope = stored(&store, "demo", &key(), "correct horse");
        envelope.version = ENVELOPE_VERSION + 1;
        let error = envelope.unlock("correct horse").unwrap_err();
        assert!(matches!(error, KeystoreError::Corrupt { .. }), "{error:?}");
    }

    #[test]
    fn an_empty_passphrase_is_refused() {
        let (_guard, store) = store();
        let error = store.store("demo", &key(), "").unwrap_err();
        assert!(matches!(error, KeystoreError::EmptyPassphrase), "{error:?}");
    }

    #[test]
    fn a_key_is_never_silently_overwritten() {
        let (_guard, store) = store();
        stored(&store, "demo", &key(), "correct horse");
        let error = store.store("demo", &key(), "correct horse").unwrap_err();
        assert!(matches!(error, KeystoreError::Exists { .. }), "{error:?}");
    }

    #[test]
    fn listing_an_absent_keystore_is_empty_not_an_error() {
        let (_guard, store) = store();
        assert!(store.list().expect("no error").is_empty());
    }

    #[test]
    fn a_missing_key_suggests_what_does_exist() {
        let (_guard, store) = store();
        stored(&store, "demo", &key(), "correct horse");
        let error = store.load("typo").unwrap_err();
        let KeystoreError::NotFound { known, .. } = &error else {
            panic!("expected NotFound, got {error:?}");
        };
        assert!(known.contains("demo"), "{known}");
    }

    #[test]
    fn labels_that_could_escape_the_keystore_are_refused() {
        for label in [
            "../escape",
            "sub/dir",
            "with space",
            "UPPER",
            "-leading-dash",
            "",
            "trailing/",
            "..",
        ] {
            assert!(
                check_label(label).is_err(),
                "`{label}` should not be a valid label"
            );
        }
        for label in ["demo", "cold-storage", "key_2", "9lives"] {
            assert!(check_label(label).is_ok(), "`{label}` should be valid");
        }
    }

    #[cfg(unix)]
    #[test]
    fn key_files_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let (_guard, store) = store();
        stored(&store, "demo", &key(), "correct horse");

        let file = std::fs::metadata(store.path_for("demo")).expect("stat");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let dir = std::fs::metadata(store.dir()).expect("stat");
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
    }
}
