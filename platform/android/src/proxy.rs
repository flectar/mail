//! Android's proxy settings, through Java's default `ProxySelector`, as the
//! platform's own HTTP stacks read them. Android keeps it in step with the
//! proxy of the network in use, with its exclusion list, and a proxy
//! auto-configuration script shows up there as a proxy Android runs on this
//! device. Android's proxies are HTTP proxies, for web requests only.

use flectar_mail::platform::SystemProxies;
use jni::{
    JNIEnv, JavaVM,
    objects::{JObject, JString, JValue},
};
use url::Url;

struct Reader {
    vm: JavaVM,
}

/// Read the system's proxy settings with `vm`. The first call counts.
pub(crate) fn install(vm: JavaVM) {
    flectar_mail::platform::configure_system_proxy(Reader { vm });
}

impl SystemProxies for Reader {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        let mut env = match self.vm.attach_current_thread() {
            Ok(env) => env,
            Err(error) => {
                tracing::warn!(%error, "could not ask Android for the proxy");
                return Vec::new();
            }
        };
        match env.with_local_frame(16, |env| select(env, url.as_str())) {
            Ok(proxies) => proxies
                .iter()
                .filter_map(|proxy| Url::parse(proxy).ok())
                .collect(),
            Err(error) => {
                let _ = env.exception_clear();
                tracing::warn!(%error, "could not ask Android for the proxy");
                Vec::new()
            }
        }
    }
}

/// `ProxySelector.getDefault().select(URI.create(url))`, as proxy URLs.
fn select(env: &mut JNIEnv, url: &str) -> jni::errors::Result<Vec<String>> {
    let text = env.new_string(url)?;
    let uri = env
        .call_static_method(
            "java/net/URI",
            "create",
            "(Ljava/lang/String;)Ljava/net/URI;",
            &[JValue::Object(&text)],
        )?
        .l()?;
    let selector = env
        .call_static_method(
            "java/net/ProxySelector",
            "getDefault",
            "()Ljava/net/ProxySelector;",
            &[],
        )?
        .l()?;
    if selector.is_null() {
        return Ok(Vec::new());
    }
    let list = env
        .call_method(
            &selector,
            "select",
            "(Ljava/net/URI;)Ljava/util/List;",
            &[JValue::Object(&uri)],
        )?
        .l()?;
    let size = env.call_method(&list, "size", "()I", &[])?.i()?;
    let mut proxies = Vec::new();
    for index in 0..size {
        let proxy = env.with_local_frame(8, |env| proxy_at(env, &list, index))?;
        proxies.extend(proxy);
    }
    Ok(proxies)
}

/// The proxy at `index` in a `List<Proxy>`, as a URL.
fn proxy_at(env: &mut JNIEnv, list: &JObject, index: i32) -> jni::errors::Result<Option<String>> {
    let proxy = env
        .call_method(list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(index)])?
        .l()?;
    let kind = env
        .call_method(&proxy, "type", "()Ljava/net/Proxy$Type;", &[])?
        .l()?;
    let kind = env
        .call_method(&kind, "name", "()Ljava/lang/String;", &[])?
        .l()?;
    let kind: String = env.get_string(&JString::from(kind))?.into();
    let scheme = match kind.as_str() {
        "DIRECT" => return Ok(Some("direct://".to_owned())),
        "HTTP" => "http",
        // Java speaks SOCKS version 5 first.
        "SOCKS" => "socks5",
        _ => return Ok(None),
    };
    let address = env
        .call_method(&proxy, "address", "()Ljava/net/SocketAddress;", &[])?
        .l()?;
    if address.is_null() || !env.is_instance_of(&address, "java/net/InetSocketAddress")? {
        return Ok(None);
    }
    let host = env
        .call_method(&address, "getHostString", "()Ljava/lang/String;", &[])?
        .l()?;
    if host.is_null() {
        return Ok(None);
    }
    let host: String = env.get_string(&JString::from(host))?.into();
    let port = env.call_method(&address, "getPort", "()I", &[])?.i()?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    Ok(Some(format!("{scheme}://{host}:{port}")))
}
