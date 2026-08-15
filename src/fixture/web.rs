use super::Fixture;

/// CSS side of the profile-card wrapper fixture.
pub const CSS: Fixture = Fixture {
    path: "web/profile-card.css",
    before: r#".profile-card {
  display: grid;
  grid-template-columns: 6rem 1fr;
  gap: 1rem;
  align-items: center;
}

.profile-card__avatar {
  width: 6rem;
  aspect-ratio: 1;
  object-fit: cover;
  border-radius: 50%;
}
"#,
    after: r#".profile-card {
  display: grid;
  grid-template-columns: 7rem 1fr;
  gap: 1rem;
  align-items: center;
}

.profile-card__portrait {
  display: grid;
  place-items: center;
  padding: 0.25rem;
  border: 1px solid #d8dee9;
  border-radius: 1rem;
}

.profile-card__portrait img {
  width: 100%;
  aspect-ratio: 1;
  object-fit: cover;
  border-radius: 0.75rem;
}
"#,
};

/// HTML side of the profile-card wrapper fixture.
pub const HTML: Fixture = Fixture {
    path: "web/profile-card.html",
    before: r#"<article class="profile-card">
  <img
    class="profile-card__avatar"
    src="/avatars/ada.webp"
    alt="Ada Lovelace"
  />
  <div>
    <h2>Ada Lovelace</h2>
    <p>First programmer</p>
  </div>
</article>
"#,
    after: r#"<article class="profile-card">
  <div class="profile-card__portrait">
    <img
      class="profile-card__avatar"
      src="/avatars/ada.webp"
      alt="Ada Lovelace"
    />
  </div>
  <div>
    <h2>Ada Lovelace</h2>
    <p>First programmer</p>
  </div>
</article>
"#,
};

/// TypeScript side of the profile-card wrapper fixture.
pub const TYPESCRIPT: Fixture = Fixture {
    path: "web/profile-card.ts",
    before: r#"export interface Profile {
  name: string;
  role: string;
  avatarUrl: string;
}

export function cardTitle(profile: Profile): string {
  return `${profile.name} — ${profile.role}`;
}

export function avatarAlt(profile: Profile): string {
  return `${profile.name}'s avatar`;
}
"#,
    after: r#"export interface Profile {
  name: string;
  role: string;
  avatarUrl: string | null;
}

export function cardTitle(profile: Profile): string {
  return `${profile.name} · ${profile.role}`;
}

export function avatarAlt(profile: Profile): string {
  return `Portrait of ${profile.name}`;
}

export function avatarSource(profile: Profile): string {
  return profile.avatarUrl ?? "/avatars/fallback.svg";
}
"#,
};

/// Lexical ribbon order for the web visual fixture.
pub const ALL: &[Fixture] = &[CSS, HTML, TYPESCRIPT];
