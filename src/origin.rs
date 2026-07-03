//! Identifies this CLI as the call surface ("origin") so chat-proxy can
//! attribute LLM usage/billing per surface (chat/craft/cli).
//! Tracking: warmblood-kr/chat-proxy#216

pub const MONOCLE_ORIGIN: &str = "cli";

pub const ORIGIN_HEADER_NAME: &str = "x-monocle-origin";

/// The origin header as a `(name, value)` pair, ready to spread into request
/// headers hitting chat-proxy.
pub fn origin_header() -> (&'static str, &'static str) {
    (ORIGIN_HEADER_NAME, MONOCLE_ORIGIN)
}

/// The standard headers for an authenticated chat-proxy call: bearer auth + the
/// origin attribution header. `bearer` must be the full `Bearer <token>` value.
/// One place owns this contract so a new header is added once, not at every site.
pub fn auth_headers(bearer: &str) -> [(&str, &str); 2] {
    [("Authorization", bearer), origin_header()]
}
