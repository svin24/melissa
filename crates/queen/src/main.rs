//! Queen bee: coordinates the hive and hosts the agent layer. Skeleton only.

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    println!("melissa queen listening on http://127.0.0.1:8080");
    melissa_web::serve(listener, melissa_web::router()).await
}
