use std::path::Path;

use rand::rngs::OsRng;

fn main() {
    let key = russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519).unwrap();
    key.write_openssh_file(Path::new("./key.pem"), russh::keys::ssh_key::LineEnding::LF)
        .unwrap();
}
