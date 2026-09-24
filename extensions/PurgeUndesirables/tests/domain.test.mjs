import assert from "node:assert/strict";
import { matchesDomain, normalizeDomain } from "../domain.js";

assert.equal(normalizeDomain("facebook.com"), "facebook.com");
assert.equal(normalizeDomain("https://www.facebook.com/messages"), "facebook.com");
assert.equal(normalizeDomain("WWW.Instagram.com"), "instagram.com");

assert.equal(matchesDomain("https://facebook.com/", "facebook.com"), true);
assert.equal(matchesDomain("https://www.facebook.com/messages/", "facebook.com"), true);
assert.equal(matchesDomain("https://m.facebook.com/", "facebook.com"), true);
assert.equal(matchesDomain("https://notfacebook.com/", "facebook.com"), false);
assert.equal(matchesDomain("https://facebook.com.evilsite.com/", "facebook.com"), false);
assert.equal(matchesDomain("https://example.com/?next=facebook.com", "facebook.com"), false);

console.log("Domain matching tests passed.");
