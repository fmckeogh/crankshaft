use std::{
    env,
    fs::File,
    io::{self, BufReader, BufWriter, Write},
    path::PathBuf,
};

pub fn main() {
    // Put the linker script somewhere the linker can find it
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(include_bytes!("memory.x"))
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=index.html");

    // Generate compressed site
    let mut input = BufReader::new(File::open("index.html").unwrap());
    let mut reader = brotli::CompressorReader::new(&mut input, 4096, 11, 21);
    let mut writer = {
        let f = File::create("index.html.br").unwrap();
        f.set_len(0).unwrap();
        BufWriter::new(f)
    };
    io::copy(&mut reader, &mut writer).unwrap();
}
