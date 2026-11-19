use std::path::PathBuf;

use clap::Parser;
use serde::Deserialize;

use crate::schema::CustomSchema;

#[derive(Parser, Debug)]
#[command(version, about = "A local Salesforce emulator for offline development and testing")]
pub struct Cli {
    /// Address to listen on.
    #[arg(long, env = "LOCALFORCE_HOST", default_value = "127.0.0.1")]
    pub host: String,
    /// HTTP port.
    #[arg(long, env = "LOCALFORCE_PORT", default_value_t = 9600)]
    pub port: u16,
    /// HTTPS port, with a generated certificate (some clients only speak https).
    #[arg(long, env = "LOCALFORCE_TLS_PORT", default_value_t = 9643)]
    pub tls_port: u16,
    /// Keep the org in this directory instead of in memory.
    #[arg(long, env = "LOCALFORCE_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Config file with users, custom objects and custom fields.
    #[arg(long, env = "LOCALFORCE_CONFIG", default_value = "localforce.toml")]
    pub config: PathBuf,
    /// SFDX project or metadata folder(s) to read custom objects and fields
    /// from, e.g. `./force-app`. Comma-separated, or repeat the flag.
    #[arg(long, env = "LOCALFORCE_METADATA", value_delimiter = ',')]
    pub metadata: Vec<PathBuf>,
    /// Reject access tokens that localforce didn't issue.
    #[arg(long, env = "LOCALFORCE_STRICT_SESSIONS")]
    pub strict_sessions: bool,
}

#[derive(Debug, Deserialize)]
pub struct UserDef {
    pub username: String,
    pub email: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    pub last_name: String,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    users: Vec<UserDef>,
    /// Metadata folders, relative to the config file.
    #[serde(default)]
    metadata: Vec<PathBuf>,
    #[serde(flatten)]
    schema: CustomSchema,
}

#[derive(Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub tls_port: u16,
    pub data_dir: Option<PathBuf>,
    pub strict_sessions: bool,
    pub metadata: Vec<PathBuf>,
    /// Where the HTTPS certificate is kept.
    pub cert_dir: PathBuf,
    pub users: Vec<UserDef>,
    pub schema: CustomSchema,
    pub clock_offset_secs: i64,
}

impl Config {
    pub fn load(cli: Cli) -> anyhow::Result<Config> {
        let file: FileConfig = match std::fs::read_to_string(&cli.config) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileConfig::default(),
            Err(e) => return Err(e.into()),
        };
        let mut users = file.users;
        if users.is_empty() {
            users.push(UserDef {
                username: "admin@localforce.dev".into(),
                email: None,
                first_name: Some("Admin".into()),
                last_name: "User".into(),
            });
        }
        if let Some(d) = &cli.data_dir {
            std::fs::create_dir_all(d)?;
        }
        Ok(Config {
            host: cli.host,
            port: cli.port,
            tls_port: cli.tls_port,
            data_dir: cli.data_dir.clone(),
            strict_sessions: cli.strict_sessions,
            metadata: {
                let base = cli.config.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let mut dirs: Vec<PathBuf> = file.metadata.iter().map(|d| base.join(d)).collect();
                dirs.extend(cli.metadata);
                dirs
            },
            cert_dir: cli.data_dir.clone().unwrap_or_else(|| std::env::temp_dir().join("localforce")),
            users,
            schema: file.schema,
            clock_offset_secs: 0,
        })
    }

    /// A config for tests: in memory, no file.
    pub fn for_tests(schema_toml: &str) -> anyhow::Result<Config> {
        let file: FileConfig = toml::from_str(schema_toml)?;
        let mut users = file.users;
        if users.is_empty() {
            users.push(UserDef {
                username: "admin@localforce.dev".into(),
                email: None,
                first_name: Some("Admin".into()),
                last_name: "User".into(),
            });
        }
        Ok(Config {
            host: "127.0.0.1".into(),
            port: 0,
            tls_port: 0,
            data_dir: None,
            strict_sessions: false,
            metadata: vec![],
            cert_dir: std::env::temp_dir().join("localforce"),
            users,
            schema: file.schema,
            clock_offset_secs: 0,
        })
    }
}
