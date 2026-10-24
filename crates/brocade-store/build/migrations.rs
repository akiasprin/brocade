use std::{fs, io, path::Path};

pub struct MigrationSource {
    pub version: i64,
    pub description: String,
    pub path: String,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn sources(directory: &Path) -> io::Result<Vec<MigrationSource>> {
    let directory = directory.canonicalize()?;
    let mut migrations = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let filename = entry.file_name();
        let filename = filename
            .to_str()
            .ok_or_else(|| invalid("migration filenames must be UTF-8"))?;
        if !entry.file_type()?.is_file() {
            return Err(invalid(format!(
                "migration {filename:?} must be a regular SQL file"
            )));
        }
        let (version, description) = filename
            .strip_suffix(".sql")
            .and_then(|name| name.split_once('_'))
            .ok_or_else(|| invalid(format!("invalid migration filename {filename:?}")))?;
        if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid(format!(
                "migration {filename:?} must have a positive integer version"
            )));
        }
        let version = version
            .parse::<i64>()
            .ok()
            .filter(|version| *version > 0)
            .ok_or_else(|| invalid(format!("migration {filename:?} has an invalid version")))?;
        if description.is_empty()
            || !description
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(invalid(format!(
                "migration {filename:?} must use <version>_<lowercase_description>.sql; only forward migrations are supported"
            )));
        }
        migrations.push(MigrationSource {
            version,
            description: description.replace('_', " "),
            path: path
                .to_str()
                .ok_or_else(|| invalid("migration paths must be UTF-8"))?
                .to_owned(),
        });
    }
    migrations.sort_by_key(|migration| migration.version);
    for pair in migrations.windows(2) {
        if pair[0].version == pair[1].version {
            return Err(invalid(format!(
                "duplicate migration version {}: {:?} and {:?}",
                pair[0].version, pair[0].path, pair[1].path
            )));
        }
    }
    if migrations
        .first()
        .is_none_or(|migration| !Path::new(&migration.path).ends_with("0001_init.sql"))
    {
        return Err(invalid("migrations must start with 0001_init.sql"));
    }
    Ok(migrations)
}
