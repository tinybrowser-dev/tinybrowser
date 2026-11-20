#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Parse,
    Early,
    Resolution,
    Runtime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Negative {
    pub error_type: Option<String>,
    pub phase: Phase,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Meta {
    pub includes: Vec<String>,
    pub flags: Vec<String>,
    pub negative: Option<Negative>,
    pub features: Vec<String>,
}

impl Meta {
    #[must_use]
    pub fn is_async(&self) -> bool {
        self.has_flag("async")
    }

    #[must_use]
    pub fn is_module(&self) -> bool {
        self.has_flag("module")
    }

    #[must_use]
    pub fn is_only_strict(&self) -> bool {
        self.has_flag("onlyStrict")
    }

    #[must_use]
    pub fn is_no_strict(&self) -> bool {
        self.has_flag("noStrict")
    }

    #[must_use]
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|declared| declared == flag)
    }
}

#[must_use]
pub fn parse(source: &str) -> Meta {
    let normalized;
    let source = if source.contains('\r') {
        normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        normalized.as_str()
    } else {
        source
    };
    let Some(start) = source.find("/*---") else {
        return Meta::default();
    };
    let after = &source[start + 5..];
    let Some(end) = after.find("---*/") else {
        return Meta::default();
    };
    let frontmatter = &after[..end];
    Meta {
        includes: values(frontmatter, "includes"),
        flags: values(frontmatter, "flags"),
        negative: negative(frontmatter),
        features: values(frontmatter, "features"),
    }
}

fn values(frontmatter: &str, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let lines: Vec<&str> = frontmatter.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let Some(rest) = line.strip_prefix(key) else {
            index += 1;
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            index += 1;
            continue;
        };
        let rest = rest.trim();
        if rest.starts_with('[') {
            let mut body = rest.trim_start_matches('[').to_owned();
            while !body.contains(']') {
                index += 1;
                let Some(next) = lines.get(index) else {
                    break;
                };
                body.push(' ');
                body.push_str(next);
            }
            let body = body.split(']').next().unwrap_or(&body);
            for item in body.split(',') {
                push_scalar(&mut values, item);
            }
        } else if rest.is_empty() {
            index += 1;
            while let Some(next) = lines.get(index) {
                let Some(item) = next.trim_start().strip_prefix("- ") else {
                    break;
                };
                push_scalar(&mut values, item);
                index += 1;
            }
            continue;
        } else {
            push_scalar(&mut values, rest);
        }
        index += 1;
    }
    values
}

fn push_scalar(values: &mut Vec<String>, item: &str) {
    let item = item.trim();
    if item.is_empty() || item.starts_with('#') {
        return;
    }
    let item = item.trim_matches(|c| c == '\'' || c == '"');
    if !item.is_empty() {
        values.push(item.to_owned());
    }
}

fn negative(frontmatter: &str) -> Option<Negative> {
    let lines: Vec<&str> = frontmatter.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with("negative:"))?;
    let mut negative = Negative {
        error_type: None,
        phase: Phase::Runtime,
    };
    for line in &lines[start + 1..] {
        if !line.starts_with([' ', '\t']) {
            break;
        }
        let line = line.trim_start();
        if let Some(value) = line.strip_prefix("type:") {
            let value = scalar(value);
            if !value.is_empty() {
                negative.error_type = Some(value);
            }
        } else if let Some(value) = line.strip_prefix("phase:") {
            negative.phase = match scalar(value).as_str() {
                "parse" => Phase::Parse,
                "early" => Phase::Early,
                "resolution" => Phase::Resolution,
                _ => Phase::Runtime,
            };
        }
    }
    Some(negative)
}

fn scalar(value: &str) -> String {
    value
        .trim()
        .trim_matches(|c| c == '\'' || c == '"')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Phase, parse};

    #[test]
    fn flow_lists_and_a_negative_block() {
        let source = r"/*---
flags: [onlyStrict, async]
includes: [assert.js, propertyHelper.js]
negative:
  phase: parse
  type: SyntaxError
---*/
";
        let meta = parse(source);
        assert!(meta.is_only_strict());
        assert!(meta.is_async());
        assert_eq!(meta.includes, ["assert.js", "propertyHelper.js"]);
        assert_eq!(
            meta.negative.as_ref().map(|negative| negative.phase),
            Some(Phase::Parse)
        );
        assert_eq!(
            meta.negative
                .as_ref()
                .and_then(|negative| negative.error_type.as_deref()),
            Some("SyntaxError")
        );
    }

    #[test]
    fn block_lists_work() {
        let source = r"/*---
includes:
  - sta.js
  - sm/assertThrowsValue.js
flags:
  - noStrict
---*/
";
        let meta = parse(source);
        assert_eq!(meta.includes, ["sta.js", "sm/assertThrowsValue.js"]);
        assert!(meta.is_no_strict());
        assert!(!meta.is_only_strict());
    }

    #[test]
    fn bare_carriage_returns_are_line_breaks() {
        let source =
            "/*---\rflags: [module]\rnegative:\r  phase: early\r  type: TypeError\r---*/\r";
        let meta = parse(source);
        assert!(meta.is_module());
        assert_eq!(
            meta.negative.as_ref().map(|negative| negative.phase),
            Some(Phase::Early)
        );
        assert_eq!(
            meta.negative
                .as_ref()
                .and_then(|negative| negative.error_type.as_deref()),
            Some("TypeError")
        );
    }

    #[test]
    fn a_file_without_frontmatter_has_no_flags() {
        let meta = parse("var x = 1;\n");
        assert_eq!(meta, super::Meta::default());
    }
}
