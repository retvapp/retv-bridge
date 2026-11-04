//! Prints the Fire TVs the SSDP/DIAL search finds (the Windows/Linux path).
use retv_bridge_mdns::{DialFinder, Interfaces, SystemDial, SystemInterfaces, DEFAULT_TIMEOUT};

#[tokio::main]
async fn main() {
    let ifaces = SystemInterfaces.ipv4();
    for s in SystemDial::default().find(&ifaces, DEFAULT_TIMEOUT).await {
        println!("{s:?}");
    }
}
