// Prints the release version the manifests and Cargo.lock agree on.
// With `--tag v<version>` it fails unless that is exactly the tag for this version.
import { args, main, version } from "./common.mjs";

main(() => {
	const options = args(process.argv.slice(2), ["tag"]);
	const releaseVersion = version();
	if (options.tag && options.tag !== `v${releaseVersion}`) throw new Error(`tag ${options.tag} does not match v${releaseVersion}`);
	console.log(releaseVersion);
});
