mod checks;
mod cli;
mod context;
mod discover;
mod doctor;
mod fs_utils;
mod git;
mod history;
mod index;
mod parse;
mod query;
mod render;
mod schema;
mod sqlite;
mod templates;
mod update;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    cli::run()
}

#[cfg(test)]
mod tests;
