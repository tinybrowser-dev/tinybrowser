pub(crate) mod store;

use std::env;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    name: ProfileName,
}

impl Profile {
    #[must_use]
    pub fn default_named() -> Self {
        Self {
            name: ProfileName::default_named(),
        }
    }

    #[must_use]
    pub fn named(name: ProfileName) -> Self {
        Self { name }
    }

    pub fn parse(raw: &str) -> Result<Self, ProfileError> {
        Ok(Self {
            name: ProfileName::parse(raw)?,
        })
    }

    #[must_use]
    pub fn name(&self) -> &ProfileName {
        &self.name
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::default_named()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn parse(raw: &str) -> Result<Self, ProfileError> {
        let name = raw.trim();
        if name.is_empty() {
            return Ok(Self::default_named());
        }
        if name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
            || name.contains('\0')
        {
            return Err(ProfileError::InvalidName);
        }
        Ok(Self(name.to_owned()))
    }

    #[must_use]
    pub fn default_named() -> Self {
        Self("default".into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    InvalidName,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => f.write_str("invalid profile name"),
        }
    }
}

impl std::error::Error for ProfileError {}

pub fn default_data_home() -> io::Result<PathBuf> {
    if let Some(dir) = env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        for var in ["LOCALAPPDATA", "APPDATA"] {
            if let Some(dir) = env::var_os(var).filter(|value| !value.is_empty()) {
                return Ok(PathBuf::from(dir));
            }
        }
        if let Some(home) = env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(home).join("AppData").join("Local"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(home)
                .join("Library")
                .join("Application Support"));
        }
    }
    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join(".local/share"));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "cannot determine data directory: XDG_DATA_HOME and HOME are unset",
    ))
}
