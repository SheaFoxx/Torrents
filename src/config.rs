use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, Write},
    path::PathBuf,
};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub max_pages: usize,
    pub entries: Vec<String>,
    pub torrents: Vec<String>,
}

impl Config {
    pub fn get_path(base_path: &String) -> Result<PathBuf> {
        let mut path = std::env::current_exe()?;
        path.set_file_name("TORRENTS");
        path.set_extension("JSON");

        let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
            bail!("Could not get current executable file name")
        };

        Ok(PathBuf::from(base_path).join(file_name))
    }

    pub fn load(base_path: &String) -> Result<Self> {
        let path = Self::get_path(base_path)?;

        let mut file = File::open(path)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        file.rewind()?;

        let config = serde_json::from_str(&text)?;

        Ok(config)
    }

    pub fn save(&mut self, base_path: &String) -> Result<()> {
        let path = Self::get_path(base_path)?;

        let mut file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        let content = serde_json::to_string_pretty(&self)?;
        file.write_all(content.as_bytes())?;

        Ok(())
    }
}
