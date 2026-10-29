//! backend-proxy の gRPC の操作（`proto/shop/v1/shop.proto`）。backend-rust はこの crate のクライアントだけを使う。
pub mod shop {
    pub mod v1 {
        tonic::include_proto!("shop.v1");
    }
}

pub use shop::v1::*;
