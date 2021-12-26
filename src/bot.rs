use std::collections::HashMap;

use crate::{
    command::Command,
    state::{AddRemovePlayerOp, AddRemovePlayerResult, Queue},
    state_container::StateContainer,
    types::{ChatId, QueueId},
    util::{fmt_naive_time, mk_players_str, mk_queue_status_msg, mk_username, send_msg},
};
use chrono::Local;
use teloxide::{prelude::*, Bot};

static INSTANT_QUEUE_TIMEOUT_MINUTES: i64 = 30;

/// Called on timed out queues. Removes the chat queue and sends an
/// informational Telegram message.
async fn handle_queue_timeout(
    sc: &StateContainer,
    bot: &Bot,
    chat_id: &ChatId,
    queue_id: &QueueId,
) -> Option<()> {
    let state = sc.read().await;

    // Remove chat queue and write new state.
    let (state, removed_queue) = state.rm_chat_queue(chat_id, queue_id);
    sc.write(state).await;

    let removed_queue = removed_queue?;

    // Inform players on Telegram about the timeout.
    let text = if removed_queue.is_full() {
        let players_str = mk_players_str(&removed_queue, true, false);
        format!("{} queue: It's time to play!\n{}", queue_id, players_str)
    } else {
        let players_str = mk_players_str(&removed_queue, false, false);
        format!("{} queue timed out!\n{}", queue_id, players_str)
    };

    send_msg(bot, chat_id, &text, false).await;

    Some(())
}

/// Task that polls and takes action for any queues that have timed out.
pub async fn poll_for_timeouts(sc: StateContainer, bot: Bot) {
    loop {
        let state = sc.read().await;
        let t = fmt_naive_time(&Local::now().time());

        // Traverse all chat queues and look for timed out queues.
        for (chat_id, chat) in &state.chats {
            for (queue_id, queue) in &chat.queues {
                // Note that we compare only HH:MM timestamps here and poll
                // every second, so we shouldn't miss any timeouts.
                if t == fmt_naive_time(&queue.timeout) {
                    handle_queue_timeout(&sc, &bot, chat_id, queue_id).await;
                }
            }
        }

        // Poll again after 1 second.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await
    }
}

/// Takes the queues and returns sorted human-readable strings with queue details.
fn make_queue_strings(queues: HashMap<QueueId, Queue>) -> Vec<String> {
    let mut queue_strings: Vec<String> = queues
        .iter()
        .map(|(queue_id, queue)| {
            let players_str = mk_players_str(queue, false, true);

            format!("{} {} {}", queue_id, players_str, queue.add_cmd)
        })
        .collect();

    queue_strings.sort();
    queue_strings
}

/// Handler for parsed incoming Telegram commands.
pub async fn handle_cmd(sc: StateContainer, bot: Bot, msg: Message, cmd: Command) -> Option<()> {
    let state = sc.read().await;
    let chat_id = ChatId::new(msg.chat.id);
    let user = msg.from()?;

    match cmd {
        Command::Help => send_msg(&bot, &chat_id, Command::descriptions(), true).await,

        Command::AddRemove { time, for_user } => {
            let username = for_user.unwrap_or_else(|| mk_username(user));

            // Construct queue_id, timeout and add_cmd based on whether command
            // targeted a timed queue or not.
            let (queue_id, timeout, add_cmd) = match time {
                Some(time) => {
                    let queue_id = QueueId::new(fmt_naive_time(&time));
                    let add_cmd = time.format("/%H%M").to_string();
                    (queue_id, time, add_cmd)
                }
                None => {
                    let queue_id = QueueId::new(String::from(""));
                    let timeout = Local::now().time()
                        + chrono::Duration::minutes(INSTANT_QUEUE_TIMEOUT_MINUTES);
                    let add_cmd = String::from("/add");
                    (queue_id, timeout, add_cmd)
                }
            };

            // Add player and update state.
            let (state, result, op) =
                state.add_remove_player(&chat_id, &queue_id, add_cmd, timeout, username);
            sc.write(state.clone()).await;

            // Construct message based on whether the queue is now full or not.
            let text = match result {
                AddRemovePlayerResult::QueueFull(queue) if queue_id.is_instant_queue() => {
                    let players_str = mk_players_str(&queue, true, false);
                    format!("Match ready in {} queue! {}", queue_id, players_str)
                }
                AddRemovePlayerResult::PlayerQueued(queue)
                | AddRemovePlayerResult::QueueFull(queue)
                | AddRemovePlayerResult::QueueEmpty(queue) => {
                    mk_queue_status_msg(&queue, &queue_id, &op)
                }
            };

            // Send queue status message.
            send_msg(&bot, &chat_id, &text, false).await;
        }

        Command::RemoveAll => {
            let username = mk_username(user);

            // Remove player and update state.
            let (state, affected_queues) = state.rm_player(&chat_id, &username);
            sc.write(state.clone()).await;

            // Send queue status message for all affected queues.
            for (queue_id, queue) in affected_queues {
                let text = mk_queue_status_msg(
                    &queue,
                    &queue_id,
                    &AddRemovePlayerOp::PlayerRemoved(username.clone()),
                );
                send_msg(&bot, &chat_id, &text, false).await
            }
        }

        Command::List => {
            let chat = state.chats.get(&chat_id);
            let queues = chat.map(|chat| chat.queues.clone());

            let current_time = Local::now().time();

            let text = match queues {
                Some(queues) if !queues.is_empty() => {
                    let (queues_today, queues_tomorrow) = queues
                        .into_iter()
                        .partition(|(_, queue)| queue.timeout > current_time);

                    let mut queue_strings = make_queue_strings(queues_today);
                    queue_strings.append(&mut make_queue_strings(queues_tomorrow));

                    queue_strings.join("\n")
                }
                _ => String::from("No active queues."),
            };

            send_msg(&bot, &chat_id, &text, false).await
        }
    }

    Some(())
}
