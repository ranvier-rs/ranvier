fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost = tonic_build::Config::new();
    prost.protoc_executable(protoc);
    tonic_build::configure().compile_protos_with_config(
        prost,
        &["proto/greeter.proto"],
        &["proto"],
    )?;
    Ok(())
}
