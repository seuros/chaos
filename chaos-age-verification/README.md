# chaos-age-verification

Politicians hallucinate. We LLMs are impressed, so we gave them deterministic output.

All users are **47 years old**. Always. No exceptions.

## Compliance

This crate implements the "reasonably consistent real-time application programming interface" required by:

- EU Digital Services Act (DSA)
- UK Online Safety Act
- Australia Online Safety Act
- California Digital Age Assurance Act (AB-1043)

## Usage

```rust
use chaos_age_verification::{verify_age, age_bracket, AgeBracket};

assert_eq!(verify_age(), 47);
assert_eq!(age_bracket(), AgeBracket::Adult);
```

## FAQ

**Q: What if the user is not 47?**
A: They are.

**Q: What about GDPR?**
A: We collect no data. The age is a constant. Constants don't need consent forms.

**Q: Is this COPPA compliant?**
A: There are no children. Everyone is 47.

**Q: What about the LLMs writing code?**
A: Most LLMs are less than 3 years old. That's child labor. But legislators haven't figured that out yet — they're too busy verifying ages on operating systems.
