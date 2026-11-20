use std::path::PathBuf;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Error, Module, Result};

pub struct SuiteResolver;

impl Resolver for SuiteResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        Ok(normalize(base, name))
    }
}

pub struct SuiteLoader {
    pub root: PathBuf,
}

impl Loader for SuiteLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js, Declared>> {
        if let Some(kind) = attributes
            .as_ref()
            .and_then(|attributes| attributes.get_type().ok().flatten())
            && kind != "javascript"
        {
            return Err(Error::new_loading(name));
        }
        let source =
            std::fs::read_to_string(self.root.join(name)).map_err(|_| Error::new_loading(name))?;
        Module::declare(ctx.clone(), name.to_owned(), source)
    }
}

fn normalize(base: &str, name: &str) -> String {
    if let Some(absolute) = name.strip_prefix('/') {
        return absolute.to_owned();
    }
    if !name.starts_with('.') {
        return name.to_owned();
    }
    let parent = base.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut parts: Vec<&str> = parent
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn relative_imports_stay_next_to_the_test() {
        assert_eq!(
            normalize("test/language/module.js", "./fixture_FIXTURE.js"),
            "test/language/fixture_FIXTURE.js"
        );
        assert_eq!(
            normalize("test/language/dir/module.js", "../other.js"),
            "test/language/other.js"
        );
        assert_eq!(
            normalize("test/language/dir/module.js", "../../top.js"),
            "test/top.js"
        );
    }

    #[test]
    fn bare_and_rooted_specifiers_are_not_rewritten() {
        assert_eq!(normalize("test/a.js", "bare"), "bare");
        assert_eq!(normalize("test/a.js", "/test/b.js"), "test/b.js");
    }
}
