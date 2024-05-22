use serenity::all::{Color, CreateEmbed, CreateEmbedAuthor, Timestamp};

const AUTHOR_NAME: &str = "Nina";
const AVATAR_IMG_URL: &str = "https://github.com/hironha/rina/static/images/nina.jpg";

#[derive(Clone, Debug)]
pub struct EmbedBuilder(CreateEmbed);

impl EmbedBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn error() -> Self {
        let embed = Self::default().0.color(Color::RED);
        Self(embed)
    }

    pub fn title(self, title: impl Into<String>) -> Self {
        let embed = self.0;
        Self(embed.title(title))
    }

    pub fn description(self, description: impl Into<String>) -> Self {
        let embed = self.0;
        Self(embed.description(description))
    }

    pub fn field(self, field: Field) -> Self {
        let embed = self.0;
        Self(embed.field(field.name, field.value, field.inline))
    }

    pub fn fields(self, fields: &[Field]) -> Self {
        let fields = fields
            .iter()
            .map(|f| (f.name.as_str(), f.value.as_str(), f.inline));

        Self(self.0.fields(fields))
    }

    pub fn build(self) -> CreateEmbed {
        self.0
    }
}

impl Default for EmbedBuilder {
    fn default() -> Self {
        let embed = CreateEmbed::new()
            .color(Color::ORANGE)
            .author(CreateEmbedAuthor::new(AUTHOR_NAME).icon_url(AVATAR_IMG_URL))
            .timestamp(Timestamp::now())
            .thumbnail(AVATAR_IMG_URL);

        Self(embed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    name: String,
    value: String,
    inline: bool,
}

impl Field {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            inline: false,
        }
    }

    pub fn inline(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            inline: true,
        }
    }
}
