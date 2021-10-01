use hif::Function;

pub enum Functions {}

#[no_mangle]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) static HIFFY_FUNCS: &[Function] = &[];

pub(crate) fn trace_execute(_offset: usize, _op: hif::Op) {}

pub(crate) fn trace_success() {}

pub(crate) fn trace_failure(_f: hif::Failure) {}
