#![warn(missing_debug_implementations, unreachable_pub)]

//! Gmail's generated resource methods exposed through Sloper capabilities.

mod client;
mod error;
mod users;

pub use error::Error;

sloper_extension::extension! {
    name: "sloper.gmail",
    actions: [users::users, users::messages, users::attachments, users::drafts,
        users::threads, users::history, users::labels, users::settings,
        users::auto_forwarding, users::filters, users::forwarding_addresses, users::send_as,
        users::delegates, users::smime_info, users::cse_identities, users::cse_keypairs],
}
