fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/ein.proto");
    tonic_build::compile_protos("proto/ein.proto")?;
    Ok(())
}
