use std::env;
use std::sync::Arc;

use reqwest::Client as HttpClient;
use serenity::all::ChannelId;
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::framework::standard::macros::{command, group};
use serenity::framework::standard::{Args, CommandResult, Configuration};
use serenity::framework::StandardFramework;
use serenity::http::Http;
use serenity::model::application::Command;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::prelude::{GatewayIntents, Mentionable, TypeMapKey};
use serenity::Result as SerenityResult;
use songbird::input::{Input, YoutubeDl};
use songbird::TrackEvent;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit};

const HELP_MESSAGE: &str = include_str!("help.md");

struct HttpKey;

impl TypeMapKey for HttpKey {
    type Value = HttpClient;
}

struct TrackTitleKey;

impl TypeMapKey for TrackTitleKey {
    type Value = String;
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        Command::set_global_commands(&ctx.http, Vec::new())
            .await
            .expect("Could not set global slash commands");

        tracing::info!("{} is connected!", ready.user.name);
    }
}

#[group]
#[commands(help, join, leave, mute, play, skip, stop, unmute)]
struct General;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_thread_ids(false)
        .with_thread_names(false)
        .compact()
        .init();

    let token = env::var("DISCORD_TOKEN").expect("Expected DISCORD_TOKEN environment variable");

    let framework = StandardFramework::new().group(&GENERAL_GROUP);
    framework.configure(Configuration::new().prefix("!"));

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird()
        .type_map_insert::<HttpKey>(HttpClient::new())
        .await
        .expect("Failed creating serenity client");

    client
        .start()
        .await
        .expect("Failed starting serenity client");
}

async fn get_http_client(ctx: &Context) -> HttpClient {
    ctx.data
        .read()
        .await
        .get::<HttpKey>()
        .cloned()
        .expect("Guaranteed to exist in the typemap.")
}

#[command]
#[only_in(guilds)]
async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let (guild_id, author_channel_id) = {
        let guild = msg.guild(&ctx.cache).expect("Expected guild to be defined");
        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|vs| vs.channel_id);

        (guild.id, channel_id)
    };

    let Some(connect_to) = author_channel_id else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    if manager.get(guild_id).is_some() {
        let res = author_channel_id
            .map(|chan| format!("Already in use at {}", chan.mention()))
            .unwrap_or_else(|| String::from("Already in use at another voice channel"));

        check_msg(msg.reply(ctx, res).await);
        return Ok(());
    }

    let Ok(voice_lock) = manager.join(guild_id, connect_to).await else {
        let res = format!("Could not join the voice channel {}", connect_to.mention());
        check_msg(msg.channel_id.say(&ctx.http, res).await);
        return Ok(());
    };

    let res = format!("Joined {}", connect_to.mention());
    check_msg(msg.channel_id.say(&ctx.http, res).await);

    let mut voice_handler = voice_lock.lock().await;
    if let Err(err) = voice_handler.deafen(true).await {
        let res = format!("Failed: {:?}", err);
        check_msg(msg.channel_id.say(&ctx.http, res).await);
        return Ok(());
    }

    Ok(())
}

struct TrackEndHandler {
    http: Arc<Http>,
    channel_id: ChannelId,
}

#[async_trait]
impl VoiceEventHandler for TrackEndHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track([(_, handle)]) = ctx {
            let typemap_read_lock = handle.typemap().blocking_read();
            let title = typemap_read_lock.get::<TrackTitleKey>();
            let message = match title {
                Some(title) => format!("Track {title} ended"),
                None => String::from("Track ended"),
            };
            check_msg(self.channel_id.say(&self.http, message).await);
        }

        None
    }
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let (guild_id, author_channel_id) = {
        let guild = msg.guild(&ctx.cache).expect("Expected guild to be defined");
        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|vs| vs.channel_id);

        (guild.id, channel_id)
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let voice_channel = voice_lock.lock().await.current_channel();
    if author_channel_id.map(songbird::id::ChannelId::from) != voice_channel {
        check_msg(msg.reply(ctx, "Not in same voice channel").await);
        return Ok(());
    }

    if let Err(e) = manager.remove(guild_id).await {
        tracing::error!("Failed leaving voice channel: {e:?}");
        let message = "Could not leave voice channel";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    let message = author_channel_id
        .map(|channel| format!("Left voice channel {}", channel.mention()))
        .unwrap_or_else(|| String::from("Left voice channel"));

    check_msg(msg.channel_id.say(&ctx.http, message).await);

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

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let mut voice_handler = voice_lock.lock().await;

    if voice_handler.is_mute() {
        check_msg(msg.channel_id.say(&ctx.http, "Already muted").await);
    } else if let Err(e) = voice_handler.mute(true).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Now muted").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn play(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let guild_id = msg.guild_id.expect("Expected guild_id to be defined");
    let Ok(music) = args.single::<String>() else {
        let res = "Must provide a music name ou URL as argument";
        check_msg(msg.channel_id.say(&ctx.http, res).await);
        return Ok(());
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization")
        .clone();

    let voice_lock = if let Some(lock) = manager.get(guild_id) {
        lock
    } else if join(ctx, msg, args.clone()).await.is_ok() {
        manager
            .get(guild_id)
            .expect("Expected voice lock after joining voice channel")
    } else {
        tracing::error!("Failed joining voice channel to play music");
        return Ok(());
    };

    let mut src: Input = if music.starts_with("http") {
        YoutubeDl::new(get_http_client(ctx).await, music).into()
    } else {
        YoutubeDl::new_search(get_http_client(ctx).await, music).into()
    };

    let metadata = match src.aux_metadata().await {
        Ok(metadata) => metadata,
        Err(err) => {
            tracing::error!("Failed loading track metadata: {err:?}");
            let message = "Track could not be loaded";
            check_msg(msg.channel_id.say(&ctx.http, message).await);
            return Ok(());
        }
    };

    let track_handle = voice_lock.lock().await.enqueue_input(src).await;

    let track_end_handler = TrackEndHandler {
        channel_id: msg.channel_id,
        http: ctx.http.clone(),
    };
    if let Err(err) = track_handle.add_event(Event::Track(TrackEvent::End), track_end_handler) {
        tracing::error!("Failed adding track end event handler: {err}");
        check_msg(msg.channel_id.say(&ctx.http, "Failed loading track").await);
        return Ok(());
    }

    let track_title = metadata
        .title
        .unwrap_or_else(|| String::from("Unknown track"));

    let message = format!("Track {track_title} added to queue");
    check_msg(msg.channel_id.say(&ctx.http, message).await);

    let mut typemap_write_lock = track_handle.typemap().blocking_write();
    typemap_write_lock.insert::<TrackTitleKey>(track_title);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn skip(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization")
        .clone();

    let Some(voice_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to play in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let voice_handler = voice_lock.lock().await;
    let queue = voice_handler.queue();
    if let Some(err) = queue.skip().err() {
        tracing::error!("Failed skipping queue song: {err}");
        return Ok(());
    }

    let message = format!(
        "Track skipped. Remaining {} track(s) in queue",
        queue.len() - 1
    );
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

    let Some(voice_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to play in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    voice_lock.lock().await.queue().stop();

    check_msg(msg.channel_id.say(&ctx.http, "Queue cleared.").await);

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

    let Some(voice_lock) = manager.get(guild_id) else {
        let message = "Not in a voice channel to unmute in";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let mut voice_handler = voice_lock.lock().await;
    if let Err(e) = voice_handler.mute(false).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Unmuted").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn help(ctx: &Context, msg: &Message) -> CommandResult {
    check_msg(msg.reply(ctx, HELP_MESSAGE).await);

    Ok(())
}

fn check_msg(result: SerenityResult<Message>) {
    if let Err(err) = result {
        tracing::error!("Error sending message: {:?}", err);
    }
}
