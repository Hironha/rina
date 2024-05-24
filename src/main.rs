mod embed;
mod playlist;

use std::env;
use std::sync::Arc;

use reqwest::Client as HttpClient;
use serenity::all::{ChannelType, CreateMessage, VoiceState};
use serenity::client::{Client, Context, EventHandler};
use serenity::framework::standard::macros::{command, group};
use serenity::framework::standard::{Args, CommandResult, Configuration};
use serenity::framework::StandardFramework;
use serenity::model::application::Command;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::prelude::{GatewayIntents, Mentionable, TypeMapKey};
use songbird::input::{Input, YoutubeDl};
use songbird::tracks::{Queued, Track, TrackHandle};
use songbird::SerenityInit;

use embed::{EmbedBuilder, EmbedField};

struct HttpKey;

impl TypeMapKey for HttpKey {
    type Value = HttpClient;
}

struct TrackTitleKey;

impl TypeMapKey for TrackTitleKey {
    type Value = Arc<str>;
}

struct Handler;

#[serenity::async_trait]
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

#[group]
#[commands(help, join, leave, mute, play, skip, stop, unmute, queue, now)]
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
        let error = EmbedBuilder::error()
            .title("!join")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Expected songbird in context");

    if manager.get(guild_id).is_some() {
        let error = EmbedBuilder::error()
            .title("!join")
            .description("I'm already in another voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(ctx, message).await);
        return Ok(());
    }

    let Ok(voice_lock) = manager.join(guild_id, connect_to).await else {
        let description = format!("Could not join the voice channel {}", connect_to.mention());
        let error = EmbedBuilder::error()
            .title("!join")
            .description(description)
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let embed = EmbedBuilder::new()
        .title("!join")
        .description(format!("Joined {}", connect_to.mention()))
        .build();

    let message = CreateMessage::new().add_embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

    let mut voice = voice_lock.lock().await;
    if let Err(err) = voice.deafen(true).await {
        tracing::error!("Failed self deafening: {err}");
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
        let error = EmbedBuilder::error()
            .title("!leave")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let voice_channel_id = voice_lock.lock().await.current_channel();
    if author_channel_id.map(songbird::id::ChannelId::from) != voice_channel_id {
        let error = EmbedBuilder::error()
            .title("!leave")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    if let Err(err) = manager.remove(guild_id).await {
        tracing::error!("Failed leaving voice channel: {err:?}");
        let error = EmbedBuilder::error()
            .title("!leave")
            .description("Failed leaving voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let voice_channel_mention = author_channel_id
        .map(|id| id.mention())
        .expect("Expected author channel id to be defined");

    let embed = EmbedBuilder::new()
        .title("!leave")
        .description(format!("Left voice channel {voice_channel_mention}"))
        .build();

    let message = CreateMessage::new().add_embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

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
        let error = EmbedBuilder::error()
            .title("!mute")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let mut voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        let error = EmbedBuilder::error()
            .title("!mute")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let embed = if voice.is_mute() {
        EmbedBuilder::new()
            .title("!mute")
            .description("I'm already muted. Use `!unmute` to unmute me")
            .build()
    } else if let Err(err) = voice.mute(true).await {
        tracing::error!("Failed self muting: {err}");

        EmbedBuilder::error()
            .title("!mute")
            .description("Could not mute myself")
            .build()
    } else {
        EmbedBuilder::new()
            .title("!mute")
            .description("I'm now muted. Use `!unmute` to unmute me")
            .build()
    };

    let message = CreateMessage::new().add_embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn play(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let (guild_id, author_channel_id) = {
        let guild = msg.guild(&ctx.cache).expect("Expected guild to be defined");
        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|vs| vs.channel_id);

        (guild.id, channel_id)
    };

    let Some(connect_to) = author_channel_id else {
        let error = EmbedBuilder::error()
            .title("!play")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let Ok(music) = args.single::<String>() else {
        let error = EmbedBuilder::error()
            .title("!play")
            .description("Missing music or URL argument")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Expected songbird in context");

    let voice_lock = if let Some(voice_lock) = manager.get(guild_id) {
        voice_lock
    } else {
        match manager.join(guild_id, connect_to).await {
            Ok(voice_lock) => voice_lock,
            Err(err) => {
                tracing::error!("Failed joining voice channel to play music: {err}");

                let description = format!("Could not join voice channel {}", connect_to.mention());
                let error = EmbedBuilder::error()
                    .title("!play")
                    .description(description)
                    .build();

                let message = CreateMessage::new().add_embed(error);
                check_msg(msg.channel_id.send_message(&ctx.http, message).await);
                return Ok(());
            }
        }
    };

    let current_channel = voice_lock.lock().await.current_channel();
    if author_channel_id.map(songbird::id::ChannelId::from) != current_channel {
        let error = EmbedBuilder::error()
            .title("!play")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    // FIXME: only works for youtube playlists, and it doesn't cover all cases
    if music.starts_with("http") && music.contains("&list=") {
        let playlist_metadata = match playlist::query(&music).await {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::error!("Failed quering playlist metadata: {err}");

                let error = EmbedBuilder::error()
                    .title("!play")
                    .description("Could not load track from playlist")
                    .build();

                let message = CreateMessage::new().add_embed(error);
                check_msg(msg.channel_id.send_message(&ctx.http, message).await);
                return Ok(());
            }
        };

        let playlist_len = playlist_metadata.len();
        let http_client = get_http_client(ctx).await;

        let mut voice = voice_lock.lock().await;
        for metadata in playlist_metadata.into_iter() {
            let src = YoutubeDl::new(http_client.clone(), metadata.url);
            let track_handle = voice.enqueue_with_preload(Track::from(src), None);
            let mut typemap = track_handle.typemap().write().await;
            typemap.insert::<TrackTitleKey>(metadata.title.into())
        }

        std::mem::drop(voice);

        let embed = EmbedBuilder::new()
            .title("!play")
            .description(format!("{playlist_len} tracks added to the queue"))
            .build();

        let message = CreateMessage::new().add_embed(embed);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let mut src: Input = if music.starts_with("http") {
        YoutubeDl::new(get_http_client(ctx).await, music).into()
    } else {
        YoutubeDl::new_search(get_http_client(ctx).await, music).into()
    };

    let metadata = src.aux_metadata().await?;
    let track_handle = voice_lock
        .lock()
        .await
        .enqueue_with_preload(Track::from(src), None);

    let mut typemap = track_handle.typemap().write().await;
    let title: Arc<str> = metadata.title.unwrap_or_else(|| "Unknown".into()).into();
    typemap.insert::<TrackTitleKey>(title);

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
        let error = EmbedBuilder::error()
            .title("!skip")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        let error = EmbedBuilder::error()
            .title("!skip")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    if voice.queue().is_empty() {
        let error = EmbedBuilder::error()
            .title("!skip")
            .description("Queue is already empty. No tracks to skip")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let amount = match args
        .single::<String>()
        .unwrap_or_else(|_| String::from("1"))
        .parse::<usize>()
    {
        Ok(amount) if amount > 20 => {
            let error = EmbedBuilder::error()
                .title("!skip")
                .description("Cannot skip more than 20 tracks at once")
                .build();

            let message = CreateMessage::new().add_embed(error);
            check_msg(msg.channel_id.send_message(&ctx.http, message).await);
            return Ok(());
        }
        Ok(amount) => amount,
        Err(_) => {
            let error = EmbedBuilder::error()
                .title("!skip")
                .description("Amount of tracks to skip must be a positive integer")
                .build();

            let message = CreateMessage::new().add_embed(error);
            check_msg(msg.channel_id.send_message(&ctx.http, message).await);
            return Ok(());
        }
    };

    let current_track = voice.queue().current();
    if let Err(err) = voice.queue().skip() {
        tracing::error!("Failed skipping current track: {err}");

        let error = EmbedBuilder::error()
            .title("!skip")
            .description("Could not skip current track")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    if amount == 1 {
        let description = match current_track {
            Some(track) => {
                let title = get_track_title(&track).await;
                format!("Current track {title} skipped")
            }
            None => String::from("Current track skipped"),
        };

        let embed = EmbedBuilder::new()
            .title("!skip")
            .description(description)
            .build();

        let message = CreateMessage::new().add_embed(embed);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let mut description = String::with_capacity((amount - 1) * 10);
    description.push_str("Skipped following tracks:\n");

    let skipped_tracks = voice
        .queue()
        .modify_queue(|q| q.drain(0..amount - 1).collect::<Vec<Queued>>());

    for (idx, track) in skipped_tracks.into_iter().enumerate() {
        let title = get_track_title(&track.handle()).await;

        description.push_str(&format!("{idx}. {title}\n"));
    }

    let embed = EmbedBuilder::new()
        .title("!skip")
        .description(description)
        .build();

    let message = CreateMessage::new().add_embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

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
        let error = EmbedBuilder::error()
            .title("!stop")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        let error = EmbedBuilder::error()
            .title("!stop")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    voice.queue().stop();

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
        let error = EmbedBuilder::error()
            .title("!unmute")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let mut voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        let error = EmbedBuilder::error()
            .title("!unmute")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let embed = if let Err(err) = voice.mute(false).await {
        tracing::error!("Failed self unmuting: {err}");

        EmbedBuilder::error()
            .title("!unmute")
            .description("Could not unmute myself")
            .build()
    } else {
        EmbedBuilder::new()
            .title("!unmute")
            .description("I'm now unmuted. Use `!mute` to mute me")
            .build()
    };

    let message = CreateMessage::new().add_embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

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
        let error = EmbedBuilder::error()
            .title("!queue")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let current_channel = voice_lock.lock().await.current_channel();
    if author_channel_id.map(songbird::id::ChannelId::from) != current_channel {
        let error = EmbedBuilder::error()
            .title("!queue")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let mut tracks = voice_lock.lock().await.queue().current_queue();
    let current_track_title = match tracks.pop() {
        Some(track) => get_track_title(&track).await,
        None => {
            let embed = EmbedBuilder::new()
                .title("!queue")
                .description("Queue is curently empty")
                .build();

            let message = CreateMessage::new().add_embed(embed);
            check_msg(msg.channel_id.send_message(&ctx.http, message).await);
            return Ok(());
        }
    };

    let len = tracks.len().max(50);
    let mut description = format!(
        "Now playing: **{current_track_title}**\n\nTotal tracks in queue: **{}**\n\n",
        tracks.len()
    );
    description.reserve(len * 10);

    for (idx, handle) in tracks.iter().take(len).enumerate() {
        let title = get_track_title(handle).await;

        description.push_str(&format!("{idx}. {title}\n"));
    }

    let embed = EmbedBuilder::new()
        .title("!queue")
        .description(description)
        .build();

    let message = CreateMessage::new().embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn now(ctx: &Context, msg: &Message) -> CommandResult {
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
        let error = EmbedBuilder::error()
            .title("!now")
            .description("User not in a voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let voice = voice_lock.lock().await;
    if author_channel_id.map(songbird::id::ChannelId::from) != voice.current_channel() {
        let error = EmbedBuilder::error()
            .title("!now")
            .description("User not in the same voice channel")
            .build();

        let message = CreateMessage::new().add_embed(error);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    }

    let Some(track_handle) = voice.queue().current() else {
        let embed = EmbedBuilder::new()
            .title("!now")
            .description("Not currently playing a track")
            .build();

        let message = CreateMessage::new().add_embed(embed);
        check_msg(msg.channel_id.send_message(&ctx.http, message).await);
        return Ok(());
    };

    let title = get_track_title(&track_handle).await;
    let embed = EmbedBuilder::new()
        .title("!now")
        .description(format!("Now playing {title}"))
        .build();

    let message = CreateMessage::new().add_embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn help(ctx: &Context, msg: &Message) -> CommandResult {
    let fields = vec![
        EmbedField::new("!help", "Explains all available commands"),
        EmbedField::new("!join", "Call **Nina** to join your current voice channel"),
        EmbedField::new("!mute", "Mutes **Nina**. Beware, if playing a track, no sound will come out. See **!unmute** to unmute **Nina**"),
        EmbedField::new("!play", "Play or enqueue a track. Must provide the track name or source **URL**"),
        EmbedField::new("!skip", "Skip track. Accepts an optional parameter to define amount of tracks to skip (max of 20)"),
        EmbedField::new("!stop", "Stop **Nina** if playing a track and clears all enqueued tracks"),
        EmbedField::new("!unmute", "Unmute **Nina**. See **!mute** to mute **Nina**"),
        EmbedField::new("!queue", "List first 50 enqueued tracks. There is currently no way to list all enqueue tracks"),
        EmbedField::new("!now", "Show playing track title"),
    ];

    let embed = EmbedBuilder::new()
        .title("!help")
        .description("Available commands")
        .fields(fields)
        .build();

    let message = CreateMessage::new().embed(embed);
    check_msg(msg.channel_id.send_message(&ctx.http, message).await);

    Ok(())
}

async fn get_http_client(ctx: &Context) -> HttpClient {
    let typemap = ctx.data.read().await;
    typemap
        .get::<HttpKey>()
        .cloned()
        .expect("HttpKey guaranteed to exist in typemap")
}

async fn get_track_title(track: &TrackHandle) -> Arc<str> {
    let typemap = track.typemap().read().await;
    typemap
        .get::<TrackTitleKey>()
        .cloned()
        .expect("Track title guaranteed to exists in typemap")
}

fn check_msg(result: serenity::Result<Message>) {
    if let Err(err) = result {
        tracing::error!("Error sending message: {:?}", err);
    }
}
