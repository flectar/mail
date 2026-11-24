//! Windows: `DnsQuery_W`, which goes through the DNS Client service and so
//! keeps the system's DNS over HTTPS and per-domain (NRPT) rules.

use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_STANDARD, DNS_RECORDW, DNS_TYPE_MX, DnsFree, DnsFreeRecordList, DnsQuery_W,
};

pub(super) fn exchanges(domain: &str) -> Option<Vec<(u16, String)>> {
    let name = domain.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut records: *mut DNS_RECORDW = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated UTF-16, and the record list is
    // freed below with DnsFree.
    let status = unsafe {
        DnsQuery_W(
            name.as_ptr(),
            DNS_TYPE_MX,
            DNS_QUERY_STANDARD,
            std::ptr::null_mut(),
            (&raw mut records).cast(),
            std::ptr::null_mut(),
        )
    };
    if status != 0 || records.is_null() {
        return None;
    }
    let mut exchanges = Vec::new();
    let mut record = records;
    while !record.is_null() {
        // SAFETY: a record of the list DnsQuery_W returned.
        let current = unsafe { &*record };
        if current.wType == DNS_TYPE_MX {
            // SAFETY: an MX record's data is DNS_MX_DATAW, whose host is a
            // NUL-terminated UTF-16 string.
            let (host, preference) = unsafe {
                let mx = current.Data.MX;
                let mut length = 0;
                while !mx.pNameExchange.is_null() && *mx.pNameExchange.add(length) != 0 {
                    length += 1;
                }
                let host = if mx.pNameExchange.is_null() {
                    String::new()
                } else {
                    String::from_utf16_lossy(std::slice::from_raw_parts(mx.pNameExchange, length))
                };
                (host, mx.wPreference)
            };
            exchanges.push((preference, host.trim_end_matches('.').to_ascii_lowercase()));
        }
        record = current.pNext;
    }
    // SAFETY: the list DnsQuery_W returned, freed once.
    unsafe { DnsFree(records.cast(), DnsFreeRecordList) };
    Some(exchanges)
}
