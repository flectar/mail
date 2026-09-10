These self-signed certificates and the accompanying private key are public test
fixtures, never production credentials. They permit deterministic local TLS
handshake tests. `server.pem` names localhost and 127.0.0.1; `wrong-host.pem` names
wrong.example. Their long expiration is intentional to avoid time-sensitive CI.
