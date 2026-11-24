//! Windows: `DnsQuery_W`, which goes through the DNS Client service and so
//! keeps the system's DNS over HTTPS and per-domain (NRPT) rules.

use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_STANDARD, DNS_RECORDW, DNS_TYPE_MX, DNS_TYPE_SRV, DNS_TYPE_TEXT, DnsFree,
    DnsFreeRecordList, DnsQuery_W,
};
use windows_sys::core::PWSTR;

use super::{Record, Service};

/// A NUL-terminated UTF-16 string from the resolver, as lowercase text.
///
/// # Safety
///
/// `text` is null or points to a NUL-terminated UTF-16 string.
unsafe fn text(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut length = 0;
    // SAFETY: the string ends with a NUL, which the loop stops at.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    }
}

fn host(name: String) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

pub(super) fn records(name: &str, kind: u16) -> Option<Vec<Record>> {
    let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut list: *mut DNS_RECORDW = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated UTF-16, and the record list is
    // freed below with DnsFree.
    let status = unsafe {
        DnsQuery_W(
            name.as_ptr(),
            kind,
            DNS_QUERY_STANDARD,
            std::ptr::null_mut(),
            (&raw mut list).cast(),
            std::ptr::null_mut(),
        )
    };
    if status != 0 || list.is_null() {
        return None;
    }
    let mut records = Vec::new();
    let mut record = list;
    while !record.is_null() {
        // SAFETY: a record of the list DnsQuery_W returned.
        let current = unsafe { &*record };
        if current.wType == kind {
            // SAFETY: the list also holds CNAME and additional records, so
            // only records of the kind asked for are read, through the union
            // member for that kind. Their names are NUL-terminated, and a
            // TXT record's string array holds `dwStringCount` strings.
            let found = unsafe {
                match kind {
                    DNS_TYPE_MX => {
                        let mx = current.Data.MX;
                        Some(Record::Mx {
                            preference: mx.wPreference,
                            host: host(text(mx.pNameExchange)),
                        })
                    }
                    DNS_TYPE_SRV => {
                        let srv = current.Data.SRV;
                        Some(Record::Srv(Service {
                            priority: srv.wPriority,
                            weight: srv.wWeight,
                            port: srv.wPort,
                            target: host(text(srv.pNameTarget)),
                        }))
                    }
                    DNS_TYPE_TEXT => {
                        // The strings follow the count, as many as it says.
                        let txt = &current.Data.TXT;
                        let first = txt.pStringArray.as_ptr();
                        let strings = (0..txt.dwStringCount as usize)
                            .map(|index| text(*first.add(index)).into_bytes())
                            .collect();
                        Some(Record::Txt(strings))
                    }
                    _ => None,
                }
            };
            records.extend(found);
        }
        record = current.pNext;
    }
    // SAFETY: the list DnsQuery_W returned, freed once.
    unsafe { DnsFree(list.cast(), DnsFreeRecordList) };
    Some(records)
}
