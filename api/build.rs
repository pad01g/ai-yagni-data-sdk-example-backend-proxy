//! proto から Rust のクライアントとサーバーを生成する。protoc は protoc-bin-vendored の同梱のものを使う
//! （ビルドする環境に protoc を入れなくてよいように）。
fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc");
    let mut config = tonic_prost_build::Config::new();
    config.protoc_executable(protoc);
    tonic_prost_build::configure()
        .compile_with_config(config, &["proto/shop/v1/shop.proto"], &["proto"])
        .expect("compile shop.proto");
    println!("cargo:rerun-if-changed=proto/shop/v1/shop.proto");
}
