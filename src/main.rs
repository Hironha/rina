use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use reqwest::Client as HttpClient;
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::framework::standard::macros::{command, group};
use serenity::framework::standard::{Args, CommandResult, Configuration};
use serenity::framework::StandardFramework;
use serenity::http::Http;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::prelude::ChannelId;
use serenity::prelude::{GatewayIntents, Mentionable, TypeMapKey};
use serenity::Result as SerenityResult;
use songbird::input::YoutubeDl;
use songbird::TrackEvent;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit};

struct HttpKey;

impl TypeMapKey for HttpKey {
    type Value = HttpClient;
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[group]
#[commands(
    deafen, join, leave, mute, play, queue, skip, stop, ping, undeafen, unmute
)]
struct General;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().expect("Failed loading .env configuration");
    tracing_subscriber::fmt()
        .with_thread_ids(false)
        .with_thread_names(false)
        .compact()
        .init();

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let framework = StandardFramework::new().group(&GENERAL_GROUP);
    framework.configure(Configuration::new().prefix("~"));

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird()
        .type_map_insert::<HttpKey>(HttpClient::new())
        .await
        .expect("Err creating client");

    let _ = client
        .start()
        .await
        .map_err(|why| println!("Client ended: {:?}", why));

    tokio::spawn(async move {
        let _ = client
            .start()
            .await
            .map_err(|why| println!("Client ended: {:?}", why));
    });

    let _signal_err = tokio::signal::ctrl_c().await;
    println!("Received Ctrl-C, shutting down.");
}

async fn get_http_client(ctx: &Context) -> HttpClient {
    let data = ctx.data.read().await;
    data.get::<HttpKey>()
        .cloned()
        .expect("Guaranteed to exist in the typemap.")
}

#[command]
async fn deafen(ctx: &Context, msg: &Message) -> CommandResult {
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(msg.guild(&ctx.cache).unwrap().id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let mut handler = handler_lock.lock().await;

    if handler.is_deaf() {
        check_msg(msg.channel_id.say(&ctx.http, "Already deafened").await);
    } else if let Err(e) = handler.deafen(true).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Deafened").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let (guild_id, channel_id) = {
        let guild = msg.guild(&ctx.cache).unwrap();
        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|voice_state| voice_state.channel_id);

        (guild.id, channel_id)
    };

    let Some(connect_to) = channel_id else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Ok(handle_lock) = manager.join(guild_id, connect_to).await else {
        let message = "Error joining the channel";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let message = format!("Joined {}", connect_to.mention());
    check_msg(msg.channel_id.say(&ctx.http, message).await);

    let track_end_event = TrackEndNotifier {
        chan_id: msg.channel_id,
        http: ctx.http.clone(),
    };

    handle_lock
        .lock()
        .await
        .add_global_event(Event::Track(TrackEvent::End), track_end_event);

    Ok(())
}

struct TrackEndNotifier {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for TrackEndNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(track_list) = ctx {
            let message = format!("Tracks ended: {}.", track_list.len());
            check_msg(self.chan_id.say(&self.http, message).await);
        }

        None
    }
}

struct ChannelDurationNotifier {
    chan_id: ChannelId,
    count: Arc<AtomicUsize>,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for ChannelDurationNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        let count_before = self.count.fetch_add(1, Ordering::Relaxed);
        let message = format!(
            "I've been in this channel for {} minutes!",
            count_before + 1
        );
        check_msg(self.chan_id.say(&self.http, message).await);

        None
    }
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = msg.guild(&ctx.cache).unwrap().id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    if manager.get(guild_id).is_none() {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    }

    if let Err(e) = manager.remove(guild_id).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Left voice channel").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn mute(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let mut handler = handler_lock.lock().await;

    if handler.is_mute() {
        check_msg(msg.channel_id.say(&ctx.http, "Already muted").await);
    } else if let Err(e) = handler.mute(true).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Now muted").await);
    }

    Ok(())
}

#[command]
async fn ping(ctx: &Context, msg: &Message) -> CommandResult {
    check_msg(msg.channel_id.say(&ctx.http, "Pong!").await);
    Ok(())
}

#[command]
#[only_in(guilds)]
async fn play(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();
    let Ok(url) = args.single::<String>() else {
        let message = "Must provide a URL to a video or audio";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    if !url.starts_with("http") {
        let message = "Must provide a valid URL";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to play in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let mut handler = handler_lock.lock().await;
    let src = YoutubeDl::new(get_http_client(ctx).await, url);

    let song_end_event = SongEndNotifier {
        chan_id: msg.channel_id,
        http: ctx.http.clone(),
    };

    let _ = handler
        .play_input(src.into())
        .add_event(Event::Track(TrackEvent::End), song_end_event);

    check_msg(msg.channel_id.say(&ctx.http, "Playing song").await);

    Ok(())
}

struct SongFader {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for SongFader {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(&[(state, track)]) = ctx {
            let _ = track.set_volume(state.volume / 2.0);

            if state.volume < 1e-2 {
                let _ = track.stop();
                check_msg(self.chan_id.say(&self.http, "Stopping song...").await);
                Some(Event::Cancel)
            } else {
                check_msg(self.chan_id.say(&self.http, "Volume reduced.").await);
                None
            }
        } else {
            None
        }
    }
}

struct SongEndNotifier {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for SongEndNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        let message = "Song faded out completely!";
        check_msg(self.chan_id.say(&self.http, message).await);

        None
    }
}

#[command]
#[only_in(guilds)]
async fn queue(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();
    let Ok(url) = args.single::<String>() else {
        let message = "Must provide a URL to a video or audio";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    if !url.starts_with("http") {
        let message = "Must provide a valid URL";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to play in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let mut handler = handler_lock.lock().await;
    let src = YoutubeDl::new(get_http_client(ctx).await, url);
    handler.enqueue_input(src.into()).await;

    let message = format!("Added song to queue: position {}", handler.queue().len());
    check_msg(msg.channel_id.say(&ctx.http, message).await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn skip(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to play in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let handler = handler_lock.lock().await;
    let queue = handler.queue();
    let _ = queue.skip();
    let message = format!("Song skipped: {} in queue.", queue.len());
    check_msg(msg.channel_id.say(&ctx.http, message).await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn stop(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to play in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let handler = handler_lock.lock().await;
    let queue = handler.queue();
    queue.stop();

    check_msg(msg.channel_id.say(&ctx.http, "Queue cleared.").await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn undeafen(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to undeafen in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let mut handler = handler_lock.lock().await;
    if let Err(e) = handler.deafen(false).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Undeafened").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn unmute(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(handler_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to unmute in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let mut handler = handler_lock.lock().await;
    if let Err(e) = handler.mute(false).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Unmuted").await);
    }

    Ok(())
}

/// Checks that a message successfully sent; if not, then logs why to stdout.
fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        println!("Error sending message: {:?}", why);
    }
}
