#[derive(Debug)]
pub struct Item {
    pub callback: String,
    pub topic: String,
    pub expires: u64,
    pub secret: Option<String>,
}

pub trait Storage {
    fn get(&self, callback: String, topic: String) -> Option<Item>;
    fn list(&self, topic: &str) -> Vec<Item>;
    fn insert(&mut self, item: Item);
    fn remove(&mut self, item: Item);
}

pub mod storages {
    use super::*;

    pub struct HashMap {
        hash_map: std::collections::HashMap<(String, String), (u64, Option<String>)>,
    }

    impl HashMap {
        pub fn new() -> Self {
            Self {
                hash_map: std::collections::HashMap::new(),
            }
        }
    }

    impl Storage for HashMap {
        fn get(&self, callback: String, topic: String) -> Option<Item> {
            println!("get {{ callback: {}, topic: {} }}", callback, topic);
            self.hash_map
                .get(&(callback.clone(), topic.clone()))
                .map(|value| Item {
                    callback,
                    topic,
                    expires: value.0,
                    secret: value.1.clone(),
                })
        }

        fn list(&self, topic: &str) -> Vec<Item> {
            self.hash_map
                .iter()
                .filter(|(key, _val)| key.1 == topic)
                .map(|(key, val)| Item {
                    callback: key.0.clone(),
                    topic: key.1.clone(),
                    expires: val.0,
                    secret: val.1.clone(),
                })
                .collect()
        }

        fn insert(&mut self, item: Item) {
            println!("insert {:?}", item);
            self.hash_map
                .insert((item.callback, item.topic), (item.expires, item.secret));
        }

        fn remove(&mut self, item: Item) {
            println!("remove {:?}", item);
            self.hash_map.remove(&(item.callback, item.topic));
        }
    }
}
