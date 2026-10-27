//! Prints the adb TVs mDNS finds on the local network.
use retv_bridge_mdns::{discover, group_services, SystemInterfaces, DEFAULT_TIMEOUT};

#[tokio::main]
async fn main() {
    match discover(DEFAULT_TIMEOUT, &SystemInterfaces).await {
        Ok(services) => {
            for s in &services {
                println!("{s:?}");
            }
            for tv in group_services(&services) {
                println!("{tv:?}");
            }
        }
        Err(e) => eprintln!("discover failed: {e}"),
    }
}
