# How-to: watch multiple repos

Add one `[[repos]]` entry per repository; `name` must equal the
payload's `repository.full_name`. Each repo gets its own pending slot;
a single global worker serializes runs (safe on one box). Same-repo
pushes may cancel their own in-flight run — never another repo's.

Run `bupha deploy webhook` once — it registers the hook on every
watched repo.
