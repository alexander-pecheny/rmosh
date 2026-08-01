fn main() {
    let protos = [
        "proto/transportinstruction.proto",
        "proto/hostinput.proto",
        "proto/userinput.proto",
    ];
    for p in protos {
        println!("cargo:rerun-if-changed={p}");
    }
    prost_build::compile_protos(&protos, &["proto"]).expect("failed to compile protobuf schemas");
}
