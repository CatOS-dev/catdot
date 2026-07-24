use catdot_core::read_user_records;
use std::path::Path;

pub fn print_system_record_diagnostics() {
    let directory = Path::new("/var/lib/catdot/users");
    if !directory.exists() {
        return;
    }
    match read_user_records(directory) {
        Ok(records) => {
            for record in records {
                let status = if uid_exists(record.uid) {
                    "valid"
                } else {
                    "missing"
                };
                println!("system user record: uid {}: {status}", record.uid);
            }
        }
        Err(error) => eprintln!("warning: cannot inspect system Catdot records: {error}"),
    }
}

fn uid_exists(uid: u32) -> bool {
    unsafe { !libc::getpwuid(uid).is_null() }
}
