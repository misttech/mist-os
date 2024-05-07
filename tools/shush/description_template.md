We're rolling out `YOUR_LINT_HERE` across the tree and adding `#[allow(YOUR_LINT_HERE)]` attributes on all current code that triggers the lint (see [here](YOUR_LINK__HERE) for more info). The following lints have been temporarily allowed in your code, and we'd like you to take a look at them:

INSERT_DETAILS_HERE

To reproduce a lint locally, remove the `#[allow]` attribute (will either be on the statement that causes the lint or the function containing it) and run `fx clippy -f $FILE`. If you need help or have questions about the lints, feel free to ask in the [discord channel](https://discordapp.com/channels/835268677472485376/940708230403866674).

If files here should not be owned by this component/owners, please add or update the relevant OWNERS file and re-assign this bug.
