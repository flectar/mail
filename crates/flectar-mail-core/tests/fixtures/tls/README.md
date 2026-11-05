These certificates and the accompanying private key are public test fixtures,
never production credentials. They permit deterministic local TLS handshake
tests. `server.pem` names localhost and 127.0.0.1; `wrong-host.pem` names
wrong.example; and `proton-bridge.pem` mirrors Proton Bridge's self-signed,
CA-marked endpoint certificate. `inspection-ca.pem` stands for a root that
antivirus or company network software installs in the system store;
`inspected-server.pem` is signed by it, names localhost and 127.0.0.1, and
uses the same key as the others. The CA's own key was discarded. Their long
expiration is intentional to avoid time-sensitive CI.
