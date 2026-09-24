import { matchesDomain } from "./domain.js";

// Deliberately use deleteUrl only. This extension never calls deleteAll/deleteRange.
const SEARCH_LIMIT = 100000;
const DELETE_BATCH_SIZE = 50;

async function deleteUrlsInBatches(urls) {
  for (let index = 0; index < urls.length; index += DELETE_BATCH_SIZE) {
    const batch = urls.slice(index, index + DELETE_BATCH_SIZE);
    await Promise.all(batch.map((url) => chrome.history.deleteUrl({ url })));
  }
}

export async function purgeDomainHistory(domain) {
  const results = await chrome.history.search({
    text: domain,
    startTime: 0,
    maxResults: SEARCH_LIMIT
  });

  const matchingUrls = [
    ...new Set(
      results
        .map((item) => item.url)
        .filter(Boolean)
        .filter((url) => matchesDomain(url, domain))
    )
  ];

  await deleteUrlsInBatches(matchingUrls);

  return {
    deletedUrls: matchingUrls.length,
    scannedResults: results.length,
    searchLimitReached: results.length >= SEARCH_LIMIT
  };
}
