use crate::logs::format_log_chat;
use log::info;
use redis::AsyncCommands;
use redis::RedisResult;
use serde::de::DeserializeOwned;
use serde::Serialize;

/* TODO: wrap connection in trait (not trivial with async). */
/// Redis connection representation.
///
/// Provides simple interface for storing sticker aliases and dialogue
/// state (with serialization).
pub struct RedisConnection {
    connection: redis::aio::Connection,
}

// General implementation
impl RedisConnection {
    /// Create new connection to redis server in specified ip.
    ///
    /// IP should be formatted according to `redis` crate requirements
    /// (currently similar to `redis://127.0.0.1/`)
    pub async fn new(redis_ip: &str) -> redis::RedisResult<RedisConnection> {
        let client = redis::Client::open(redis_ip)?;
        let con = client.get_async_connection().await?;
        Ok(RedisConnection { connection: con })
    }

    /// Get redis key for chat given its identifier.
    fn get_chat_key(chat_id: i64) -> String {
        format!("chat:{}", chat_id)
    }
}

impl RedisConnection {
    /// Get redis key for aliases storage.
    fn get_aliases_key(chat_id: i64) -> String {
        RedisConnection::get_chat_key(chat_id) + "aliases"
    }

    /// Store alias-sticker mapping in redis.
    ///
    /// If the alias is already tied to some sticker, overwrite it so the alias will be mapped to a new
    /// sticker (for given `chat_id`).
    pub async fn set_alias(&mut self, chat_id: i64, alias: &str, sticker_id: &str) {
        let key: String = RedisConnection::get_aliases_key(chat_id);
        let set_result: RedisResult<()> = self.connection.hset(key, alias, sticker_id).await;
        match set_result {
            Ok(_) => {
                info!(
                    "{}",
                    format_log_chat(
                        &format!("Saved alias '{a}' for '{s}'", a = alias, s = sticker_id),
                        chat_id
                    )
                );
            }
            Err(e) => {
                info!(
                    "{}",
                    format_log_chat(&format!("Failed to save alias to DB: {}", e), chat_id)
                );
            }
        }
    }

    /// Set multiple aliases for a sticker.
    pub async fn set_aliases<'a, T>(&mut self, chat_id: i64, aliases: T, sticker_id: &str)
    where
        T: IntoIterator<Item = &'a str>,
    {
        for alias in aliases {
            self.set_alias(chat_id, alias, sticker_id).await;
        }
    }

    /// Obtain sticker id for given alias in the chat (if any).
    pub async fn get_sticker_id(&mut self, chat_id: i64, alias: &str) -> Option<String> {
        let key: String = RedisConnection::get_aliases_key(chat_id);
        let set_result: RedisResult<String> = self.connection.hget(key, alias).await;
        match set_result {
            Ok(sticker_id) => {
                info!(
                    "{}",
                    format_log_chat(
                        &format!("Retrieved '{s}' by alias '{a}'", a = alias, s = sticker_id),
                        chat_id
                    )
                );
                Some(sticker_id)
            }
            Err(e) => {
                info!(
                    "{}",
                    format_log_chat(
                        &format!("Failed find alias '{a}' in DB: {}", e, a = alias),
                        chat_id
                    )
                );
                None
            }
        }
    }

    // TODO: add support of sticker removal
    #[allow(dead_code)]
    /// Unmap (remove) the alias for given chat id.
    pub async fn remove_alias(&mut self, chat_id: i64, alias: &str) {
        let key: String = RedisConnection::get_aliases_key(chat_id);
        let del_result: RedisResult<()> = self.connection.hdel(key, alias).await;
        match del_result {
            Ok(_) => {
                info!(
                    "{}",
                    format_log_chat(&format!("Removed alias '{a}'", a = alias), chat_id)
                );
            }
            Err(e) => {
                info!(
                    "{}",
                    format_log_chat(&format!("Failed to remove alias from DB: {}", e), chat_id)
                );
            }
        }
    }
}

/// An error returned from `Storage` implementation.
#[derive(Debug)]
pub enum RedisStorageError {
    SerdeError(serde_json::Error),

    RedisError(redis::RedisError),

    /// Returned from [`RedisStorage::remove_dialogue`].
    DialogueNotFound,
}

/// Dialogue storage.
///
/// Similar to `teloxide::dispatching::dialogue::Storage`, but with different dialogue for each user
/// in the chat.
impl RedisConnection {
    /// Get redis key for dialogues storage for given chat id.
    fn get_dialogues_key(chat_id: i64) -> String {
        RedisConnection::get_chat_key(chat_id) + "dialogues"
    }

    /// Get field name for given from_id (can be empty).
    fn get_from_field(from_id: Option<i64>) -> String {
        from_id
            .map(|x| x.to_string())
            .unwrap_or("NO_ID".to_string())
    }

    /// Update a dialogue in the storage.
    ///
    /// Saves the `dialogue` in the redis database for given chat and user.
    pub async fn update_dialogue<'a, D>(
        &mut self,
        chat_id: i64,
        from_id: Option<i64>,
        dialogue: D,
    ) -> Result<(), RedisStorageError>
    where
        D: Serialize,
    {
        let key: String = RedisConnection::get_dialogues_key(chat_id);
        let field: String = RedisConnection::get_from_field(from_id);

        // Serialize
        let value: String = serde_json::to_string(&dialogue).map_err(|err| {
            info!(
                "{}",
                format_log_chat(&format!("Failed to serialize dialogue: {}", err), chat_id)
            );
            RedisStorageError::SerdeError(err)
        })?;

        // Save
        let set_result: RedisResult<()> = self.connection.hset(&key, &field, &value).await;
        match &set_result {
            Ok(_) => {
                info!(
                    "{}",
                    format_log_chat(&format!("Saved dialogue for '{f}'", f = field), chat_id)
                );
            }
            Err(err) => {
                info!(
                    "{}",
                    format_log_chat(&format!("Failed to save dialogue to DB: {}", err), chat_id)
                );
            }
        }
        set_result.map_err(RedisStorageError::RedisError)
    }

    /// Retrieve a dialogue from the storage.
    ///
    /// Givethe `dialogue` for given chat and user.
    pub async fn get_dialogue<'a, D>(
        &mut self,
        chat_id: i64,
        from_id: Option<i64>,
    ) -> Result<Option<D>, RedisStorageError>
    where
        D: DeserializeOwned,
    {
        let key: String = RedisConnection::get_dialogues_key(chat_id);
        let field: String = RedisConnection::get_from_field(from_id);

        // Retrieve from DB
        let value: Option<String> = self
            .connection
            .hget(&key, &field)
            .await
            .map_err(RedisStorageError::RedisError)?;

        // Deserialize
        let value: Result<Option<D>, RedisStorageError> = value
            .map(|v| serde_json::from_str::<D>(&v[..]))
            .transpose()
            .map_err(RedisStorageError::SerdeError);
        value
    }

    /// Remove dialogue.
    pub async fn remove_dialogue(
        &mut self,
        chat_id: i64,
        from_id: Option<i64>,
    ) -> Result<(), RedisStorageError> {
        let key: String = RedisConnection::get_dialogues_key(chat_id);
        let field: String = RedisConnection::get_from_field(from_id);

        let del_res: RedisResult<i64> = self.connection.hdel(key, field).await;
        match del_res {
            Ok(0) => Err(RedisStorageError::DialogueNotFound),
            Ok(_) => Ok(()),
            Err(e) => Err(RedisStorageError::RedisError(e)),
        }
    }
}
