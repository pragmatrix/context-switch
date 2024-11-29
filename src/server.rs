use std::collections::HashMap;

use crate::endpoint::{Conversation, Endpoint};

struct Server {
    /// All the registered endpoints on this server.
    endpoints: HashMap<String, Box<dyn Endpoint>>,
    /// Running conversations.
    conversations: HashMap<String, Box<dyn Conversation>>,
}

