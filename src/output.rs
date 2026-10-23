use std::fmt;
use std::io::{self, Write};

// Keep the renderers' line-oriented interface without std's panic on write errors.
macro_rules! println {
    () => { $crate::output::print_stdout(format_args!("\n")) };
    ($($arg:tt)*) => {
        $crate::output::print_stdout(format_args!("{}\n", format_args!($($arg)*)))
    };
}

macro_rules! eprintln {
    () => { $crate::output::print_stderr(format_args!("\n")) };
    ($($arg:tt)*) => {
        $crate::output::print_stderr(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use eprintln;
pub(crate) use println;

pub(crate) fn print_stdout(args: fmt::Arguments<'_>) {
    let mut stdout = io::stdout().lock();
    finish_write(stdout.write_fmt(args).and_then(|_| stdout.flush()));
}

pub(crate) fn print_stderr(args: fmt::Arguments<'_>) {
    let mut stderr = io::stderr().lock();
    finish_write(stderr.write_fmt(args).and_then(|_| stderr.flush()));
}

pub(crate) fn finish_write(result: io::Result<()>) {
    if let Err(error) = result {
        if error.kind() == io::ErrorKind::BrokenPipe {
            crate::usage::exit(0);
        }
        // Reporting an output failure must not itself panic if stderr is closed.
        let _ = writeln!(io::stderr().lock(), "Error: {}", error);
        crate::usage::exit(2);
    }
}
