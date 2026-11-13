//! Which variable names read like a secret (E-12): the one definition. An isolated run never
//! passes such a variable into its container, and a build block whose output holds such a
//! variable's value, when the variable isn't in the block's key, never leaves the machine (B-24).

/// Credentials by name: hushrun's and CI's, and the common ones whose names say nothing.
const SECRET_NAMES: &[&str] = &[
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_RUNTIME_URL",
    "AMQP_URL",
    "CI_JOB_JWT",
    "CI_JOB_JWT_V2",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "HUSHRUN_AGENT",
    "MONGODB_URI",
    "MONGO_URL",
    "REDIS_URL",
    "SSH_AUTH_SOCK",
];
const SECRET_PARTS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "CREDENTIAL",
    "PRIVATE",
    "AUTH",
    "ACCESS_KEY",
    "APIKEY",
    "DATABASE_URL",
    "_KEY_",
];
const SECRET_SUFFIXES: &[&str] = &["_KEY", "_PAT", "_DSN", "_PWD"];

/// Whether `name` reads like a secret (E-12): one of the credentials above by name, a name
/// with one of the parts above in it, or one ending in one of the suffixes. Case doesn't matter.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAMES.contains(&upper.as_str())
        || SECRET_PARTS.iter().any(|part| upper.contains(part))
        || SECRET_SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: E-12, B-24
    #[test]
    fn names_that_read_like_secrets() {
        for name in [
            "GITHUB_TOKEN",
            "CI_JOB_TOKEN",
            "db_password",
            "STRIPE_API_KEY",
            "SENTRY_DSN",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["GITHUB_SERVER_URL", "PATH", "KEYBOARD", "RUST_LOG"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }
}
