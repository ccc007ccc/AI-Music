//! SFZ v2 preprocessing kept behind the sampler boundary.
//!
//! This module only expands source text. It does not decide how an opcode
//! sounds, which keeps include/macro/path policy independent from voice
//! allocation. The implementation deliberately supports the preprocessing
//! constructs used by the Salamander definitions while retaining strict path
//! and recursion limits.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

const MAX_INCLUDE_DEPTH: usize = 32;
const MAX_EXPANDED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum PreprocessError {
    #[error("could not read included SFZ {path:?} (line {line}): {source}")]
    ReadFile {
        path: PathBuf,
        line: usize,
        #[source]
        source: std::io::Error,
    },
    #[error("SFZ include path {path:?} (line {line}) must stay inside the asset pack")]
    InvalidIncludePath { path: PathBuf, line: usize },
    #[error("SFZ include cycle at {path:?} (line {line})")]
    IncludeCycle { path: PathBuf, line: usize },
    #[error("SFZ include depth exceeds {MAX_INCLUDE_DEPTH} (line {line})")]
    IncludeDepth { line: usize },
    #[error("SFZ expanded source exceeds {MAX_EXPANDED_BYTES} bytes")]
    ExpandedTooLarge,
    #[error("SFZ line {line}: malformed {directive} directive")]
    MalformedDirective {
        line: usize,
        directive: &'static str,
    },
}

pub(crate) fn expand(entry: &Path, pack_root: &Path) -> Result<String, PreprocessError> {
    let mut state = State {
        pack_root: pack_root.to_owned(),
        include_root: entry.parent().unwrap_or_else(|| Path::new(".")).to_owned(),
        defines: HashMap::new(),
        stack: Vec::new(),
        output: String::new(),
    };
    state.expand_file(entry, 0, 0)?;
    Ok(state.output)
}

struct State {
    pack_root: PathBuf,
    /// SFZ include paths are resolved relative to the main instrument file,
    /// even when the included file lives in a subdirectory.
    include_root: PathBuf,
    defines: HashMap<String, String>,
    stack: Vec<PathBuf>,
    output: String,
}

impl State {
    fn expand_file(
        &mut self,
        path: &Path,
        depth: usize,
        include_line: usize,
    ) -> Result<(), PreprocessError> {
        if depth > MAX_INCLUDE_DEPTH {
            return Err(PreprocessError::IncludeDepth { line: include_line });
        }
        let canonical = fs::canonicalize(path).map_err(|source| PreprocessError::ReadFile {
            path: path.to_owned(),
            line: include_line,
            source,
        })?;
        if !canonical.starts_with(&self.pack_root) {
            return Err(PreprocessError::InvalidIncludePath {
                path: canonical,
                line: include_line,
            });
        }
        if self.stack.iter().any(|active| active == &canonical) {
            return Err(PreprocessError::IncludeCycle {
                path: canonical,
                line: include_line,
            });
        }
        let source =
            fs::read_to_string(&canonical).map_err(|source| PreprocessError::ReadFile {
                path: canonical.clone(),
                line: include_line,
                source,
            })?;
        self.stack.push(canonical.clone());
        for (index, original_line) in source.lines().enumerate() {
            self.expand_line(original_line, index + 1, depth)?;
            if self.output.len() > MAX_EXPANDED_BYTES {
                self.stack.pop();
                return Err(PreprocessError::ExpandedTooLarge);
            }
        }
        self.stack.pop();
        Ok(())
    }

    fn expand_line(
        &mut self,
        original_line: &str,
        line: usize,
        depth: usize,
    ) -> Result<(), PreprocessError> {
        let source_line = strip_comment(original_line);
        if source_line.trim().is_empty() {
            return Ok(());
        }

        // Definitions are normally standalone, but accepting a prefix/suffix
        // makes the same logic work for compact SFZ files.
        if let Some(index) = find_directive(&source_line, "#define") {
            let prefix = &source_line[..index];
            let rest = source_line[index + "#define".len()..].trim_start();
            let mut parts = rest.splitn(2, char::is_whitespace);
            let Some(name) = parts.next().filter(|name| name.starts_with('$')) else {
                return Err(PreprocessError::MalformedDirective {
                    line,
                    directive: "#define",
                });
            };
            let Some(remainder) = parts.next().map(str::trim_start) else {
                return Err(PreprocessError::MalformedDirective {
                    line,
                    directive: "#define",
                });
            };
            let mut value_parts = remainder.splitn(2, char::is_whitespace);
            let Some(value) = value_parts.next().filter(|value| !value.is_empty()) else {
                return Err(PreprocessError::MalformedDirective {
                    line,
                    directive: "#define",
                });
            };
            self.defines
                .insert(name[1..].to_owned(), self.substitute(value));
            let suffix = value_parts.next().unwrap_or_default();
            if !prefix.trim().is_empty() || !suffix.trim().is_empty() {
                self.emit_line(&format!(
                    "{}{}",
                    self.substitute(prefix),
                    self.substitute(suffix)
                ));
            }
            return Ok(());
        }

        if let Some(index) = find_directive(&source_line, "#include") {
            let prefix = &source_line[..index];
            let rest = source_line[index + "#include".len()..].trim_start();
            let (include_name, suffix) =
                parse_include_argument(rest).ok_or(PreprocessError::MalformedDirective {
                    line,
                    directive: "#include",
                })?;
            let include_name = self.substitute(include_name);
            let include_path = PathBuf::from(include_name);
            if !is_safe_relative_path(&include_path) {
                return Err(PreprocessError::InvalidIncludePath {
                    path: include_path,
                    line,
                });
            }
            let include_path = self.include_root.join(include_path);
            if !prefix.trim().is_empty() {
                self.emit_line(&self.substitute(prefix));
            }
            self.expand_file(&include_path, depth + 1, line)?;
            if !suffix.trim().is_empty() {
                self.expand_line(suffix, line, depth)?;
            }
            return Ok(());
        }

        self.emit_line(&self.substitute(&source_line));
        Ok(())
    }

    fn substitute(&self, source: &str) -> String {
        let mut output = String::with_capacity(source.len());
        let mut cursor = 0;
        while cursor < source.len() {
            let bytes = source.as_bytes();
            if bytes[cursor] == b'$' {
                let start = cursor + 1;
                let mut end = start;
                while end < source.len()
                    && (source.as_bytes()[end].is_ascii_alphanumeric()
                        || source.as_bytes()[end] == b'_')
                {
                    end += 1;
                }
                if end > start {
                    let name = &source[start..end];
                    if let Some(value) = self.defines.get(name) {
                        output.push_str(value);
                    } else {
                        output.push_str(&source[cursor..end]);
                    }
                    cursor = end;
                    continue;
                }
            }
            let character = source[cursor..].chars().next().unwrap();
            output.push(character);
            cursor += character.len_utf8();
        }
        output
    }

    fn emit_line(&mut self, line: &str) {
        self.output.push_str(line.trim_end());
        self.output.push('\n');
    }
}

fn strip_comment(line: &str) -> String {
    let mut quoted = false;
    let bytes = line.as_bytes();
    for index in 0..line.len().saturating_sub(1) {
        match bytes[index] {
            b'"' => quoted = !quoted,
            b'/' if !quoted && bytes[index + 1] == b'/' => return line[..index].to_owned(),
            _ => {}
        }
    }
    line.to_owned()
}

fn find_directive(line: &str, directive: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(relative) = line[offset..].find(directive) {
        let index = offset + relative;
        let before_ok = index == 0
            || line.as_bytes()[index - 1].is_ascii_whitespace()
            || line.as_bytes()[index - 1] == b'>';
        let after = index + directive.len();
        let after_ok = after >= line.len() || line.as_bytes()[after].is_ascii_whitespace();
        if before_ok && after_ok {
            return Some(index);
        }
        offset = after;
        if offset >= line.len() {
            return None;
        }
    }
    None
}

fn parse_include_argument(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.trim_start();
    let quoted = rest.strip_prefix('"')?;
    let end = quoted.find('"')?;
    let name = &quoted[..end];
    Some((name, &quoted[end + 1..]))
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);

    impl Directory {
        fn create() -> Self {
            loop {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!("ai-music-sfz-pre-{stamp}-{id}"));
                if fs::create_dir(&path).is_ok() {
                    return Self(path);
                }
            }
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn expands_nested_includes_and_macros() {
        let directory = Directory::create();
        fs::create_dir(directory.0.join("Data")).unwrap();
        fs::write(
            directory.0.join("main.sfz"),
            "#define $EXT wav\n<control> default_path=Samples/\n#include \"Data/part.sfz\"\n",
        )
        .unwrap();
        fs::write(
            directory.0.join("Data/part.sfz"),
            "#define $NAME C4\n<region> sample=$NAME.$EXT key=$NAME\n",
        )
        .unwrap();
        let output = expand(&directory.0.join("main.sfz"), &directory.0).unwrap();
        assert!(output.contains("sample=C4.wav key=C4"));
        assert!(output.contains("default_path=Samples/"));
    }

    #[test]
    fn resolves_nested_include_paths_from_the_main_instrument_directory() {
        let directory = Directory::create();
        fs::create_dir(directory.0.join("Data")).unwrap();
        fs::create_dir(directory.0.join("Data/nested")).unwrap();
        fs::write(directory.0.join("main.sfz"), "#include \"Data/part.sfz\"\n").unwrap();
        fs::write(
            directory.0.join("Data/part.sfz"),
            "#include \"Data/nested/leaf.sfz\"\n",
        )
        .unwrap();
        fs::write(directory.0.join("Data/nested/leaf.sfz"), "<region>\n").unwrap();
        let output = expand(&directory.0.join("main.sfz"), &directory.0).unwrap();
        assert!(output.contains("<region>"));
    }

    #[test]
    fn rejects_include_traversal_and_cycles() {
        let directory = Directory::create();
        fs::write(
            directory.0.join("main.sfz"),
            "#include \"../outside.sfz\"\n",
        )
        .unwrap();
        assert!(matches!(
            expand(&directory.0.join("main.sfz"), &directory.0),
            Err(PreprocessError::InvalidIncludePath { .. })
        ));
        fs::write(directory.0.join("a.sfz"), "#include \"b.sfz\"\n").unwrap();
        fs::write(directory.0.join("b.sfz"), "#include \"a.sfz\"\n").unwrap();
        assert!(matches!(
            expand(&directory.0.join("a.sfz"), &directory.0),
            Err(PreprocessError::IncludeCycle { .. })
        ));
    }
}
