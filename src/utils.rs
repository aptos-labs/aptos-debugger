use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceLoc {
    pub path: PathBuf,
    pub line: u32,
}

impl SourceLoc {
    pub fn new(path: &Path, line: u32) -> Self {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        Self { path, line }
    }

    pub fn to_dap_source(&self) -> dap::types::Source {
        let name = self
            .path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned());
        dap::types::Source {
            name,
            path: Some(self.path.to_string_lossy().into_owned()),
            ..Default::default()
        }
    }
}

impl std::fmt::Display for SourceLoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.path.display(), self.line)
    }
}

pub(crate) fn trim_hex_address(name: &str) -> String {
    let Some((first, rest)) = name.split_once("::") else {
        return name.to_string();
    };
    let had_prefix = first.starts_with("0x");
    let hex_part = first.strip_prefix("0x").unwrap_or(first);
    if !hex_part.chars().all(|c| c.is_ascii_hexdigit()) || hex_part.is_empty() {
        return name.to_string();
    }
    let trimmed = hex_part.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    let prefix = if had_prefix { "0x" } else { "00" };
    format!("{}{}::{}", prefix, trimmed, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trim_hex_address() {
        assert_eq!(
            trim_hex_address("0x0000000000000000000000000000000000000042::simple::test"),
            "0x42::simple::test"
        );
        assert_eq!(
            trim_hex_address("0x0000000000000000000000000000000000000001::module::func"),
            "0x1::module::func"
        );
        assert_eq!(trim_hex_address("0xABCD::foo"), "0xABCD::foo");

        assert_eq!(
            trim_hex_address(
                "0000000000000000000000000000000000000000000000000000000000000042::simple::2"
            ),
            "0042::simple::2"
        );
        assert_eq!(
            trim_hex_address("0000000000000000000000000000000000000001::module::func"),
            "001::module::func"
        );
        assert_eq!(
            trim_hex_address("0000000000000000000000000000000000000000::module::func"),
            "000::module::func"
        );

        assert_eq!(
            trim_hex_address("0x0000000000000000000000000000000000000000"),
            "0x0000000000000000000000000000000000000000"
        );
        assert_eq!(trim_hex_address("0x00ff"), "0x00ff");

        assert_eq!(trim_hex_address("no_hex_here::func"), "no_hex_here::func");
    }
}
