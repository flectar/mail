//! Sieve scripts on the user's mail server (#239), managed over ManageSieve
//! (RFC 5804).

pub(crate) mod managesieve;

#[cfg(test)]
mod test_server;
