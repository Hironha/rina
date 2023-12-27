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
use serenity::utils::MessageBuilder;
use serenity::Result as SerenityResult;
use songbird::input::{AuxMetadata, Input, YoutubeDl};
use songbird::TrackEvent;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit};

struct HttpKey;

impl TypeMapKey for HttpKey {
    type Value = HttpClient;
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
        .with_thread_ids(false)
        .with_thread_names(false)
        .compact()
        .init();

    if dotenvy::dotenv().is_err() {
        tracing::error!("Failed loading .env configuration")
    }

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
        .map_err(|why| tracing::error!("Client ended: {:?}", why))
        .expect("Failed starting serenity client");

    tokio::spawn(async move {
        client
            .start()
            .await
            .map_err(|why| tracing::error!("Client ended: {:?}", why))
            .expect("Failed starting serenity client");
    });

    tracing::info!("Received Ctrl-C, shutting down.");
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
        let guild = msg.guild(&ctx.cache).unwrap();
        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|voice_state| voice_state.channel_id);

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
        let message = author_channel_id
            .map(|chan| format!("Already in use at {}", chan.mention()))
            .unwrap_or_else(|| String::from("Already in use at another voice channel"));

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

    let mut voice_handler = voice_lock.lock().await;
    if let Err(e) = voice_handler.deafen(true).await {
        let message = format!("Failed: {:?}", e);
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    Ok(())
}

struct TrackEndHandler {
    http: Arc<Http>,
    channel_id: ChannelId,
    track_metadata: AuxMetadata,
}

#[async_trait]
impl VoiceEventHandler for TrackEndHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(..) = ctx {
            let message = if let Some(title) = &self.track_metadata.title {
                format!("Track {} ended", title)
            } else {
                tracing::error!(
                    "No track metadata found. Following metadata: {:?}",
                    self.track_metadata
                );
                String::from("Track ended")
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
        let guild = msg
            .guild(&ctx.cache)
            .expect("Could not get guild from serenity context cache");

        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|voice_state| voice_state.channel_id);

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
    let guild_id = msg.guild_id.unwrap();
    let Ok(url) = args.single::<String>() else {
        let message = "Must provide a URL to a video or audio";
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    };

    if !url.starts_with("http") {
        let message = format!("{url} is not a valid URL");
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization")
        .clone();

    let voice_lock = if let Some(lock) = manager.get(guild_id) {
        lock
    } else if join(ctx, msg, args.clone()).await.is_ok() {
        manager
            .get(guild_id)
            .expect("Voice lock expected after joining voice channel")
    } else {
        tracing::error!("Failed joining voice channel to play music");
        return Ok(());
    };

    let mut src: Input = YoutubeDl::new(get_http_client(ctx).await, url).into();
    let metadata = match src.aux_metadata().await {
        Ok(metadata) => metadata,
        Err(err) => {
            tracing::error!("Failed loading track metadata: {err:?}");
            let message = "Track could not be loaded";
            check_msg(msg.channel_id.say(&ctx.http, message).await);
            return Ok(());
        }
    };

    let track_title = metadata.title.clone();
    let track_end_handler = TrackEndHandler {
        channel_id: msg.channel_id,
        http: ctx.http.clone(),
        track_metadata: metadata,
    };

    voice_lock
        .lock()
        .await
        .enqueue_input(src)
        .await
        .add_event(Event::Track(TrackEvent::End), track_end_handler)
        .expect("Failed adding track end event");

    let message = track_title
        .map(|title| format!("Track {title} added to queue"))
        .unwrap_or_else(|| String::from("Track added to queue"));

    check_msg(msg.channel_id.say(&ctx.http, message).await);

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
    // TODO: improve message implementation, maybe using `include_str!` macro
    let message = MessageBuilder::new()
        .push_bold_line("!help")
        .push_line("Explains all available commands\n")
        .push_bold_line("!join")
        .push_line("Calls **Rina** to join your current voice channel\n")
        .push_bold_line("!leave")
        .push_line("Removes **Rina** from current voice channel and clears track queue\n")
        .push_bold_line("!mute")
        .push_line("Mutes **Rina**. Beware, if playing a track, no sound will come out\n")
        .push_bold_line("!play")
        .push_line("Must provide an **url** as argument. If **Rina** is not in a voice channel yet, it's going to join your current voice channel and enqueue the provided track in **url**. Automatically searching track by name is not implemented yet\n")
        .push_bold_line("!skip")
        .push_line("Skips current track\n")
        .push_bold_line("!stop")
        .push_line("Stops playing tracks, also clears all tracks in queue\n")
        .push_bold_line("!unmute")
        .push_line("Unmutes **Rina**")
        .build();

    check_msg(msg.reply(ctx, message).await);

    Ok(())
}

fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        tracing::error!("Error sending message: {:?}", why);
    }
}
