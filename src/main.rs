use std::env;
use std::sync::Arc;

use reqwest::Client as HttpClient;
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::framework::standard::macros::{command, group};
use serenity::framework::standard::{Args, CommandResult, Configuration};
use serenity::framework::StandardFramework;
use serenity::http::Http;
use serenity::model::application::Command;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::prelude::ChannelId;
use serenity::prelude::{GatewayIntents, Mentionable, TypeMapKey};
use serenity::Result as SerenityResult;
use songbird::input::YoutubeDl;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit};
use songbird::{Songbird, TrackEvent};

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
#[commands(join, leave, mute, play, skip, stop, ping, unmute)]
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

    let track_end_handler = TrackEndNotifier {
        manager,
        chan_id: msg.channel_id,
        http: ctx.http.clone(),
    };

    voice_handler.add_global_event(Event::Track(TrackEvent::End), track_end_handler);

    Ok(())
}

struct TrackEndNotifier {
    chan_id: ChannelId,
    http: Arc<Http>,
    manager: Arc<Songbird>,
}

#[async_trait]
impl VoiceEventHandler for TrackEndNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::DriverDisconnect(data) => {
                if let Err(err) = self.manager.remove(data.guild_id).await {
                    tracing::error!("Failed removing voice handler on disconnect: {err:?}");
                }
            }
            EventContext::Track(track_list) => {
                if track_list.get(0).is_some() {
                    let message = "Track ended";
                    check_msg(self.chan_id.say(&self.http, message).await);
                } else {
                    tracing::error!("Track end event dispatched but there is no track attached");
                }
            }
            _ => {}
        };

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

    let Some(voice_lock) = manager.get(guild_id) else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
        return Ok(());
    };

    voice_lock.lock().await.remove_all_global_events();

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
        let message = format!("{url} is not a valid URL");
        check_msg(msg.channel_id.say(&ctx.http, message).await);
        return Ok(());
    }

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.")
        .clone();

    let voice_lock = if let Some(lock) = manager.get(guild_id) {
        lock
    } else if join(ctx, msg, args.clone()).await.is_ok() {
        let Some(lock) = manager.get(guild_id) else {
            tracing::error!("Could not get voice handler even after joining");
            return Ok(());
        };

        lock
    } else {
        tracing::error!("Failed joining voice channel to play music");
        return Ok(());
    };

    
    let src = YoutubeDl::new(get_http_client(ctx).await, url);
    voice_lock.lock().await.enqueue_input(src.into()).await;
    check_msg(msg.channel_id.say(&ctx.http, "Added song to queue").await);

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

fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        tracing::error!("Error sending message: {:?}", why);
    }
}
