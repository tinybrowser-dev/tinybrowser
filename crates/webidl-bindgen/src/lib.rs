mod contracts;
mod database;
mod emit;
mod javascript;
mod model;
mod names;

pub struct Source<'source> {
    pub name: &'source str,
    pub text: &'source str,
}

pub fn validate_sources(sources: &[Source<'_>]) -> Result<(), Error> {
    database::Database::parse(sources).map(|_| ())
}

pub struct ReflectedEnum {
    pub interface: String,
    pub attribute: String,
    pub content: String,
    pub type_path: String,
    pub missing: Option<String>,
    pub invalid: Option<String>,
}

pub fn parse_reflected_enumerated(source: &Source<'_>) -> Result<Vec<ReflectedEnum>, Error> {
    crate::contracts::parse_reflected_enumerated(source)
}

pub struct Binding {
    pub interface: String,
    pub rust: String,
}

pub fn compile_contracts(idl: &[Source<'_>], rust: &[Source<'_>]) -> Result<Vec<Binding>, Error> {
    contracts::compile(idl, rust, &[])
}

pub fn compile_contracts_with_reflection(
    idl: &[Source<'_>],
    rust: &[Source<'_>],
    reflected: &[ReflectedEnum],
) -> Result<Vec<Binding>, Error> {
    contracts::compile(idl, rust, reflected)
}

pub struct Javascript {
    pub source: String,
    pub interfaces: Vec<String>,
}

pub fn compile_javascript(idl: &[Source<'_>], source: &Source<'_>) -> Result<Javascript, Error> {
    javascript::compile(idl, source)
}

use std::fmt;

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
