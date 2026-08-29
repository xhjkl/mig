use super::Fixture;

/// CSS side of the metasyntactic wrapper fixture.
pub const CSS: Fixture = Fixture {
    path: "alpha.css",
    before: r#".alpha {
  display: grid;
  grid-template-columns: 6rem 1fr;
  gap: 1rem;
  align-items: center;
}

.alpha__beta {
  width: 6rem;
  aspect-ratio: 1;
  object-fit: cover;
  border-radius: 50%;
}
"#,
    after: r#".alpha {
  display: grid;
  grid-template-columns: 7rem 1fr;
  gap: 1rem;
  align-items: center;
}

.alpha__gamma {
  display: grid;
  place-items: center;
  padding: 0.25rem;
  border: 1px solid #d8dee9;
  border-radius: 1rem;
}

.alpha__gamma img {
  width: 100%;
  aspect-ratio: 1;
  object-fit: cover;
  border-radius: 0.75rem;
}
"#,
};

/// HTML side of the metasyntactic wrapper fixture.
pub const HTML: Fixture = Fixture {
    path: "alpha.html",
    before: r#"<article class="alpha">
  <img
    class="alpha__beta"
    src="alpha.webp"
    alt="Alpha"
  />
  <div>
    <h2>Beta</h2>
    <p>Gamma</p>
  </div>
</article>
"#,
    after: r#"<article class="alpha">
  <div class="alpha__gamma">
    <img
      class="alpha__beta"
      src="alpha.webp"
      alt="Alpha"
    />
  </div>
  <div>
    <h2>Beta</h2>
    <p>Gamma</p>
  </div>
</article>
"#,
};

/// TypeScript side of the metasyntactic wrapper fixture.
pub const TYPESCRIPT: Fixture = Fixture {
    path: "alpha.ts",
    before: r#"export interface Alpha {
  beta: string;
  gamma: string;
  delta: string;
}

export function beta(alpha: Alpha): string {
  return `${alpha.beta} — ${alpha.gamma}`;
}

export function gamma(alpha: Alpha): string {
  return `${alpha.beta}: ${alpha.gamma}`;
}
"#,
    after: r#"export interface Alpha {
  beta: string;
  gamma: string;
  delta: string | null;
}

export function beta(alpha: Alpha): string {
  return `${alpha.beta} · ${alpha.gamma}`;
}

export function gamma(alpha: Alpha): string {
  return `Gamma: ${alpha.beta}`;
}

export function delta(alpha: Alpha): string {
  return alpha.delta ?? "delta.svg";
}
"#,
};

/// Lexical ribbon order for the web visual fixture.
pub const ALL: &[Fixture] = &[CSS, HTML, TYPESCRIPT];
