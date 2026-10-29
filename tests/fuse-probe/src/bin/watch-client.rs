//! What a file manager does: an inotify watch on a directory, every event printed.
//! `watch-client <dir> <seconds> [states]`: with `states`, read the state on each ATTRIB.
//! Reading the attribute never opens content.

use std::ffi::CString;

fn now_ms() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    assert!(a.len() == 3 || (a.len() == 4 && a[3] == "states"), "watch-client <dir> <seconds> [states]");
    let states = a.len() == 4;
    let (dir, secs): (&str, u64) = (&a[1], a[2].parse().unwrap());
    unsafe {
        let fd = libc::inotify_init1(0);
        let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_ATTRIB | libc::IN_MODIFY | libc::IN_MOVED_FROM | libc::IN_MOVED_TO;
        assert!(libc::inotify_add_watch(fd, CString::new(dir).unwrap().as_ptr(), mask) >= 0);
        if states { println!("watching"); }
        let end = now_ms() + secs * 1000;
        let mut buf = [0u8; 4096];
        while now_ms() < end {
            let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            if libc::poll(&mut p, 1, 200) <= 0 {
                continue;
            }
            let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
            let mut off = 0;
            while off < n as usize {
                let ev = &*(buf.as_ptr().add(off) as *const libc::inotify_event);
                // An event on the watched directory itself has no name. Looking past
                // its header would read the next event or a former name left in buf.
                let name = if ev.len == 0 {
                    std::borrow::Cow::Borrowed("")
                } else {
                    std::ffi::CStr::from_ptr(buf.as_ptr().add(off + 16).cast()).to_string_lossy()
                };
                let kind = [(libc::IN_CREATE, "CREATE"), (libc::IN_DELETE, "DELETE"), (libc::IN_ATTRIB, "ATTRIB"), (libc::IN_MODIFY, "MODIFY")]
                    .iter()
                    .filter(|(m, _)| ev.mask & m != 0)
                    .map(|(_, n)| *n)
                    .collect::<Vec<_>>()
                    .join("|");
                println!("inotify: {kind} {name}");
                if states && ev.mask & libc::IN_ATTRIB != 0 {
                    let path = std::path::Path::new(dir).join(name.as_ref());
                    let path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                    let mut value = [0u8; 64];
                    let size = libc::getxattr(path.as_ptr(), c"user.konedrive.state".as_ptr(), value.as_mut_ptr().cast(), value.len());
                    if size >= 0 { println!("state: {name} {}", String::from_utf8_lossy(&value[..size as usize])); }
                }
                off += 16 + ev.len as usize;
            }
        }
    }
}
