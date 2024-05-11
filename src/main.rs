mod playlist;

use std::env;

use reqwest::Client as HttpClient;
use serenity::all::{ChannelType, VoiceState};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::framework::standard::macros::{command, group};
use serenity::framework::standard::{Args, CommandResult, Configuration};
use serenity::framework::StandardFramework;
use serenity::model::application::Command;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::prelude::{GatewayIntents, Mentionable, TypeMapKey};
use songbird::input::{Input, YoutubeDl};
use songbird::tracks::Queued;
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

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        let channel_id = match (old.and_then(|state| state.channel_id), new.channel_id) {
            // if old state has channel_id and new state doesn't, it means the user left voice channel
            (Some(channel_id), None) => channel_id,
            _ => return tracing::info!("Voice state updated, but not a leave event"),
        };

        let Some(guild_id) = new.guild_id else {
            return tracing::error!("Unexpected guild_id not defined in new state");
        };

        let channels = match guild_id.channels(&ctx.http).await {
            Ok(channels) => channels,
            Err(err) => return tracing::error!("Failed getting guild channels: {err:?}"),
        };

        let voice_channel = match channels.get(&channel_id) {
            Some(channel) if channel.kind == ChannelType::Voice => channel,
            Some(_) => return tracing::info!("No voice channel defined for {channel_id}"),
            None => return tracing::info!("No channel defined for {channel_id}"),
        };

        let members = match voice_channel.members(&ctx.cache) {
            Ok(members) => members,
            Err(err) => return tracing::error!("Failed getting members from channel: {err:?}"),
        };

        if members.len() > 1 {
            let remaining = members.len();
            return tracing::info!("Remaining {remaining} members connected to voice channel");
        }

        let manager = songbird::get(&ctx)
            .await
            .expect("Expected songbird in context");

        if let Err(err) = manager.remove(voice_channel.guild_id).await {
            return tracing::error!("Failed leaving empty voice channel automatically: {err:?}");
        }
    }
}

struct TrackEndHandler;

#[async_trait]
impl VoiceEventHandler for TrackEndHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track([(_, handle)]) = ctx {
            let typemap = handle.typemap().read().await;
            match typemap.get::<TrackTitleKey>() {
                Some(title) => tracing::info!("Track {title} ended"),
                None => tracing::info!("Track ended"),
            };
        }

        None
    }
}

#[group]
#[commands(help, join, leave, mute, play, skip, stop, unmute, queue)]
struct General;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
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
        .expect("Expected songbird in context");

    if manager.get(guild_id).is_some() {
        let message = "Already in use at another voice channel";
        check_msg(msg.reply(ctx, message).await);
        return Ok(());
    }

    let Ok(voice_lock) = manager.join(guild_id, connect_to).await else {
        let message = format!("Could not join the voice channel {}", connect_to.mention());
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let message = format!("Joined {}", connect_to.mention());
    check_msg(msg.channel_id.say(&ctx.http, message).await);

    let mut voice = voice_lock.lock().await;
    if let Err(err) = voice.deafen(true).await {
        let message = format!("Failed: {:?}", err);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    Ok(())
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
        .expect("Expected songbird in context");

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let voice_channel_id = voice_lock.lock().await.current_channel();
    if author_channel_id.map(songbird::id::ChannelId::from) != voice_channel_id {
        check_msg(msg.reply(ctx, "Not in same voice channel").await);
        return Ok(());
    }

    if let Err(err) = manager.remove(guild_id).await {
        tracing::error!("Failed leaving voice channel: {err:?}");
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
        .expect("Expected songbird in context");

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let mut voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        check_msg(msg.reply(ctx, "Not in same voice channel").await);
        return Ok(());
    }

    if voice.is_mute() {
        check_msg(msg.channel_id.say(&ctx.http, "Already muted").await);
    } else if let Err(e) = voice.mute(true).await {
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
        let message = "Must provide a music name ou URL as argument";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Expected songbird in context");

    let voice_lock = if let Some(voice_lock) = manager.get(guild_id) {
        voice_lock
    } else if join(ctx, msg, args).await.is_ok() {
        manager
            .get(guild_id)
            .expect("Expected voice lock after joining voice channel")
    } else {
        tracing::error!("Failed joining voice channel to play music");
        return Ok(());
    };

    // FIXME: only works for youtube playlists, and it doesn't cover all cases
    if music.starts_with("http") && music.contains("&list=") {
        let playlist_metadata = match playlist::query(&music).await {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::error!("Failed quering playlist metadata: {err}");
                let message = "Could not get playlist information";
                check_msg(msg.reply(ctx, message).await);
                return Ok(());
            }
        };
        let playlist_len = playlist_metadata.len();
        let mut voice = voice_lock.lock().await;
        let http_client = get_http_client(ctx).await;

        for metadata in playlist_metadata.into_iter() {
            let src: Input = YoutubeDl::new(http_client.clone(), metadata.url).into();
            voice.enqueue_input(src).await;
        }

        let message = format!("{playlist_len} tracks added to the queue");
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

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
    if let Err(err) = track_handle.add_event(Event::Track(TrackEvent::End), TrackEndHandler) {
        tracing::error!("Failed adding track end event handler: {err}");
        check_msg(msg.channel_id.say(&ctx.http, "Failed loading track").await);
        return Ok(());
    }

    let mut typemap = track_handle.typemap().write().await;
    typemap.insert::<TrackTitleKey>(metadata.title.unwrap_or_else(|| String::from("Unknown")));

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn skip(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
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
        .expect("Expected songbird in context");

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        check_msg(msg.reply(ctx, "Not in same voice channel").await);
        return Ok(());
    }

    let queue = voice.queue();
    if queue.is_empty() {
        let message = "Queue is empty. No tracks to skip";
        check_msg(msg.reply(&ctx.http, message).await);
        return Ok(());
    }

    let amount = match args
        .single::<String>()
        .unwrap_or_else(|_| String::from("1"))
        .parse::<usize>()
    {
        Ok(amount) => amount,
        Err(_) => {
            let message = "Amount of tracks to skip must be a non zero positive int";
            check_msg(msg.reply(&ctx.http, message).await);
            return Ok(());
        }
    };

    if let Err(err) = queue.skip() {
        tracing::error!("Failed skipping current track: {err}");
        let message = "Could not skip current track";
        check_msg(msg.reply(&ctx.http, message).await);
        return Ok(());
    }

    if amount == 1usize {
        // TODO: add track title in
        let message = "Skipped playing track";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    let mut message = String::with_capacity(amount * 10);
    message.push_str("Skipped following tracks:\n");

    let skipped_tracks = queue.modify_queue(|q| q.drain(0..amount).collect::<Vec<Queued>>());
    let total_skipped = skipped_tracks.len();
    for (idx, track) in skipped_tracks.into_iter().enumerate() {
        let handle = track.handle();
        let typemap = handle.typemap().read().await;
        let title = typemap
            .get::<TrackTitleKey>()
            .cloned()
            .expect("Track title guaranteed to exists in typemap");

        let label = match idx {
            idx if idx == total_skipped - 1 => format!("{idx}. {title}"),
            idx => format!("{idx}. {title}\n"),
        };

        message.push_str(&label);
    }

    check_msg(msg.channel_id.say(&ctx.http, message).await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn stop(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
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
        .expect("Expected songbird in context");

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        check_msg(msg.reply(ctx, "Not in same voice channel").await);
        return Ok(());
    }

    voice.queue().stop();

    check_msg(msg.channel_id.say(&ctx.http, "Queue cleared.").await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn unmute(ctx: &Context, msg: &Message) -> CommandResult {
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
        .expect("Expected songbird in context");

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    let mut voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        check_msg(msg.reply(ctx, "Not in same voice channel").await);
        return Ok(());
    }

    if let Err(err) = voice.mute(false).await {
        let message = format!("Failed: {:?}", err);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Unmuted").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn queue(ctx: &Context, msg: &Message) -> CommandResult {
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
        .expect("Expected songbird in context");

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(&ctx.http, "Not in a voice channel").await);
        return Ok(());
    };

    let voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        check_msg(msg.reply(ctx, "Not in same voice channel").await);
        return Ok(());
    }

    let tracks = voice.queue().current_queue();
    if tracks.is_empty() {
        let message = "Queue is currently empty";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    let mut message = String::with_capacity(tracks.len() * 10);
    for (idx, handle) in tracks.iter().enumerate() {
        let typemap = handle.typemap().read().await;
        let title = typemap
            .get::<TrackTitleKey>()
            .cloned()
            .expect("Track title guaranteed to exists in typemap");

        let label = match idx {
            0 => format!("Now playing: {title}\n"),
            i if i == tracks.len() - 1 => format!("{i}. {title}"),
            i => format!("{i}. {title}\n"),
        };

        message.push_str(&label);
    }

    check_msg(msg.channel_id.say(&ctx.http, message).await);
    Ok(())
}

#[command]
#[only_in(guilds)]
async fn help(ctx: &Context, msg: &Message) -> CommandResult {
    check_msg(msg.reply(ctx, HELP_MESSAGE).await);

    Ok(())
}

async fn get_http_client(ctx: &Context) -> HttpClient {
    let typemap = ctx.data.read().await;
    typemap
        .get::<HttpKey>()
        .cloned()
        .expect("HttpKey guaranteed to exist in typemap")
}

fn check_msg(result: serenity::Result<Message>) {
    if let Err(err) = result {
        tracing::error!("Error sending message: {:?}", err);
    }
}
