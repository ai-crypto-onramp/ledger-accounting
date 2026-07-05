use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

fn app() -> Router {
    Router::new().route("/healthz", get(healthz))
}

#[tokio::main]
async fn main() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app()).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let Json(val) = healthz().await;
        assert_eq!(val, json!({ "status": "ok" }));
    }
}