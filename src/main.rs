mod checks;
mod cli;
mod context;
mod discover;
mod doctor;
mod external_refs;
mod fs_utils;
mod git;
mod history;
mod index;
mod lifecycle;
mod output;
mod parse;
mod query;
mod render;
mod schema;
mod sqlite;
mod templates;
mod update;
mod usage;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    usage::start();
    if let Err(error) = cli::run() {
        if error
            .downcast_ref::<std::io::Error>()
            .map(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
            .unwrap_or(false)
        {
            usage::finish(0);
            return;
        }
        output::eprintln!("Error: {}", error);
        usage::exit(2);
    }
    usage::finish(0);
}

#[cfg(test)]
mod tests;
