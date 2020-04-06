# First sprint recap and reflection

Okay! I now have:

- A proposed syscall layer that actually bottoms out to `SVC` instructions.
- Some Rust sugar atop that.
- A hello world.
- An aggressively simplified 16550 driver.
- A fake build environment that produces Cortex-M4 ELF binaries. Which are
  linked wrong. But ignore that.

# Binary size

- `test0` comes in at 524 bytes text and uses no RAM outside the stack.
- `uart_driver` is 604 bytes text and burns some RAM to model fake hardware
  registers.

The size of `test0` is driven by, hilariously, `memset`. It lends out a
1024-byte buffer, which it initializes, and that's big enough that the compiler
decides to call out to `memset`. The Cortex-M4 LLVM `memset` implementation is
designed for speed over size. Anyway, more on this in a bit.

The size of `uart_driver` is actually being driven by main itself, despite
having code paths that panic, thanks to `panic_halt` and its super-efficient
panic impl. Main is 542 bytes of code.

Oh hey we're compiling at `O3`.

```
  opt-level=|  O3 |  Os |  Oz
test0       | 524 | 496 | 492
uart_driver | 604 | 596 | 572
```

Cool. These sizes are fine for now.

# Overall thoughts

The current API and programming model makes it pretty easy to write simple
programs. I feel like this is a good goal. Minimizing "framework" code helps
with binary size and programmer understanding.

Contrast with Tock, where writing a minimal blinky turns out to have a _lot of
steps._ (Though that's not a totally fair comparison since Hubris doesn't even
have a timer yet.)

# On the syscall interface

## send

Shape is OK but I'm concerned about performance. Ought to be able to get more
parameters into registers. We minimally need

- Target task ID
- Request pointer
- Request length
- Response pointer
- Response length
- Lease count
- Perhaps a non-blocking flag

(I've listed lease count, but not lease _pointer_, because I expect that a lot
of messages will not lend anything. We can determine that by checking the count.
The lease _pointer_ can come from e.g. the caller stack, where fetching it is
more expensive -- but it only needs to be fetched for non-zero counts.)

I would kind of like to make operation selector into another first-class field,
so that we don't have to place it at the start of every message.

If we limited the maximum lease count to something reasonable, like 255, we
could probably pack (task ID, lease count, flags, operation selector) into a
single register, which gets us down to five registers. We happen to have five
caller-save registers to work with on ARMv7-M (r0-3 and r12). Unfortunately,
the calling convention only gives us four register parameters, so to pass more
values in registers I'd need inline asm with register constraints/clobbers.

## send failures

Currently sends can fail for two reasons:

1. Dead peer.
2. Peer attempted to send a response that was too large.

There is an open question on whether messaging a dead peer should be a fault
(i.e. a condition that must be dealt with at a supervisor) or a locally
recoverable error. Local error is more flexible; it can be converted to a fault
(by panicking) but the opposite is hard.

Given that `receive` informs a server of how much reply space its caller has
allocated, it might be reasonable to eliminate the "response too large" error in
favor of a fault _at the server._ The server appears to be willfully
misbehaving.

## Safe send/receive

We can currently send/receive slices of bytes. Great. But something more
pleasant would be good. Both the message and the response ought to be modeled as
actual types, like structs.

It turns out there's some serious subtlety here related to the definition of
memory safety.

One part of Rust's memory safety guarantee is that types that have fewer than
`2^number_of_bits` valid representations won't contain invalid values. Take
`bool`. `bool` is at least 8 bits long, but the only valid values are 0 and 1;
safe Rust assumes and preserves this property.

Okay, so back to IPC. Imagine you're implementing a server that takes three
different messages. I'd be strongly inclined to try implementing it like this:

```rust
enum MyProtocol {
    Foo(u32),
    Bar(bool),
    Baz(u8, u8),
}

match receive() {
    MyProtocol::Foo(x) => ...,
    // and so forth
}
```

However, making this happen in safe Rust is actually kind of involved, because
(like bool) you can't just interpret an arbitrary section of memory as an enum
like this. It isn't safe; you could wind up with an invalid enum discriminator.
(Nevermind that the enum shown above has no defined in-memory representation,
which is a whole other issue I'll address elsewhere.)

This is also probably not what you want in terms of storage: the messages will
all be the size of the *largest enum variant*. Why spend a `u32` worth of space
to send `Bar`'s lone `bool`?

The right thing to do is:

1. Define each message as a separate type, so they have separate sizes.
2. Receive a message.
3. Inspect its selector, reject it if out-of-range.
4. Check the length of the message against the type implied by that selector.
5. Turn the bytes into the message type somehow.

There are traits, defined in the `zerocopy` crate, for types that can fulfill
step 5 above. Specifically, that would want the `FromBytes` trait. `zerocopy`
has operations for checking length and casting types that implement `FromBytes`,
so it covers steps 4 *and* 5. (You'd probably also want the message type to
implement `AsBytes` for the sender.)

In doing this, we've lost the ability to ergonomically and safely use `match`.
One possibility, which adds some ergonomics back while minimizing copies, would
resemble the following:

```rust
#[derive(AsBytes, FromBytes)]
struct Foo(u32);

#[derive(AsBytes, FromBytes)]
struct Bar(bool);

#[derive(AsBytes, FromBytes)]
struct Baz(u8, u8);

enum MyProtocol<'a> {
    Foo(&'a Foo),
    Bar(&'a Bar),
    Baz(&'a Baz),
}

fn specialized_receive(buffer: &[u8]) -> Result<MyProtocol<'_>, DecodeError> {
    // pretend this is generated dispatch and checking code
}

match specialized_receive(&my_buffer)? {
    MyProtocol::Foo(foo) => ...,
    // and so forth
}
```

`MyProtocol` here combines the results from step 3 and step 5 above: it pairs an
operation selector (the enum variant) with a *typed* reference to an incoming
message. You'd obviously want to generate this code, writing it out would be
tedious.

Another option would be to map the operations onto trait methods. This doesn't
let us use `match`, but *does* let us write each operation as a separate
function, which may wind up being easier to read and reason about.

```rust
// structs Foo, Bar, Baz defined as above

trait MyProtocolServer {
    fn foo(&Foo);
    fn bar(&Bar);
    fn baz(&Baz);

    fn unrecognized(&[u8]);
}

// Server implementation needs a dummy type to impl the trait on. You could also
// go OO and put the server state inside this type.

struct Server;

impl MyProtocolServer for Server {
    fn foo(msg: &Foo) {
        // ...
    }
    // and so forth
}
```

# Faults and stuff

I've been alluding to an idea of how faults work in my head, but I don't think
I've written it down. It's already influencing the design, so better get it on
"paper."

A task can **fault**. The processor defines some kinds of faults: accessing a
memory address outside of the task's memory map, for example, or executing an
undefined or privileged instruction.

Hubris defines additional types of faults at the kernel level. For example,
blatantly misusing a kernel API when the program had enough information to do it
correctly indicates a programmer error and likely malfunction, and is modeled as
a software-generated fault.

Finally, tasks can initiate faults _themselves_. I expect we will wind up with a
`panic!` handler that converts it into a fault.

## What happens on a fault

High level:

- Faulting task is stopped.
- Kernel-side task control information is updated to record fault info.
- The task's supervisor is notified and can take action (dump state, restart,
  reboot computer, etc)

The task's supervisor is another task, given by ID, and fixed at compile time.
The kernel has this information.

How is the supervisor notified? I am currently chewing on two alternative
approaches:

1. **Notification.** The kernel sets a notification bit in the supervisor. The
   supervisor can then detect this when it next receives. On detecting the
   condition, it will need to make a kernel call to identify faulting tasks,
   since the notification bits may not convey enough information to identify the
   culprit.

2. **Message.** The faulting task behaves as though it had sent a message to its
   supervisor. This will be received by the supervisor, subject to
   prioritization, at its next receive. Since the shape of the message is
   determined by the kernel, it needs to be distinguishable from other messages
   the supervisor might receive.

The first one is simple, is similar to how we handle interrupts, etc.

The second one is probably faster (i.e. the faulting task can be conveyed in the
message instead of requiring a separate scan), and -- importantly -- _can be
faked by a task._ If a task can send the supervisor a message identical to the
one that would result if it faulted, we can use that to implement `panic!`.

This of course requires the task to have the ability to send to the supervisor,
which might or might not make sense in every application. (Though I expect it to
be fairly common for responding to health inquiries.)

## What events cause faults

On ARMv7-M we have
- MPU access violation
- Stack overflow (new in ARMv8-M)
- Bus fault (i.e. doing something dumb with device memory)
- Usage fault (illegal instruction, privilege violation)

On RISC-V I suspect the set is similar. These all indicate misbehavior or
malfunction, unless you are using MPU access violations to implement paging,
which we are not.

Defined in the kernel, we have:

- Attempt to send a message over the system maximum message size.
- Bogus message sent to kernel task.
- IPC MAC filter violation (e.g. trying to send to a forbidden destination).
- Attempt to loan out memory you do not own.
- Invalid syscall.
- Attempt to reply to a message with an overly long response (probably).
- Probably some stuff related to the yet-unimplemented SENDNB/SENDA proposed
  operations.

Again, these are all programmer errors in kernel or inter-task interaction where
the program had enough information to know better. I'm conservatively treating
them all as malfunctions.

And finally, at the application level, we have:

- `panic!`

Which, again, programmer error or state corruption.

# Next steps

- Rearrange send parameters to make it more efficient, measure impact on binary
  sizes.
- Consider allowing reply buffer and receive target buffer to be `MaybeUninit`
  to eliminate the need to clear them.
- See about distinguishing operation selector from the rest of the message and
  applying this change to the demo protocols.
