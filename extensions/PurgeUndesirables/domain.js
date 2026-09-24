export function normalizeDomain(input) {
  const raw = input.trim();
  if (!raw) {
    throw new Error("Enter a domain first.");
  }

  const candidate = /^[a-z][a-z\d+.-]*:\/\//i.test(raw)
    ? raw
    : `https://${raw}`;

  let url;
  try {
    url = new URL(candidate);
  } catch {
    throw new Error("That does not look like a valid domain.");
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Only http/https websites are supported.");
  }

  let hostname = url.hostname.toLowerCase().replace(/\.$/, "");
  if (hostname.startsWith("www.")) {
    hostname = hostname.slice(4);
  }

  if (!hostname || (!hostname.includes(".") && hostname !== "localhost")) {
    throw new Error("Enter a hostname such as facebook.com.");
  }

  return hostname;
}

export function matchesDomain(urlString, domain) {
  try {
    const url = new URL(urlString);
    const hostname = url.hostname.toLowerCase().replace(/\.$/, "");
    const normalizedDomain = domain.toLowerCase().replace(/^www\./, "").replace(/\.$/, "");

    return hostname === normalizedDomain || hostname.endsWith(`.${normalizedDomain}`);
  } catch {
    return false;
  }
}
