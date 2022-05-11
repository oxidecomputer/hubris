use userlib::*;
use ringbuf::*;

ringbuf!(Option<CrashReport>, 16, None);

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct CrashReport {
    pub task: u16,
    pub fault: abi::FaultInfo,
    pub msg: [u8; 128],
    pub msg_len: u8,
}

pub fn report_fault(task: usize, fault: &abi::FaultInfo) {
    let mut report = CrashReport {
        task: task as u16,
        fault: fault.clone(),
        msg: [0; 128],
        msg_len: 0,
    };
    if let abi::FaultInfo::Panic = fault {
        let msg = kipc::read_task_panic_message(task, &mut report.msg);
        report.msg_len = u8::try_from(msg.len()).unwrap_or(255);
    }

    ringbuf_entry!(Some(report));
}
