use std::{fs, io, time::Instant};

use spellbook::Dictionary;

macro_rules! usage {
    () => {
        eprintln!("Usage: load-dictionary path/to/dict.aff path/to/dict.dic");
        eprintln!("  Note: some shells accept a syntax like path/to/dict.{{aff,dic}}");
        std::process::exit(1);
    };
}

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(aff) = args.next() else {
        usage!();
    };
    let Some(dic) = args.next() else {
        usage!();
    };
    let aff = fs::read_to_string(aff)?;
    let dic = fs::read_to_string(dic)?;
    let now = Instant::now();
    match Dictionary::new(&aff, &dic) {
        Ok(_) => println!("Compiled the dictionary in {:?}", now.elapsed()),
        Err(err) => eprintln!("Failed to compile the dictionary: {err}"),
    }
    Ok(())
}
