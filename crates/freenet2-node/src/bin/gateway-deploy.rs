use freenet2_node::*;
use libp2p::identity::Keypair;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = Keypair::generate_ed25519();
    let mut node = NodeConfig::default().with_key(key).build_libp2p()?;
    node.listen_on().map_err(|_| "failed to start".into())
}
