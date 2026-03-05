import { tool } from "@opencode-ai/plugin"

export default tool({
  description: `Fetch a web article and extract its main content as clean markdown.
Uses the Readability algorithm (same as Firefox Reader View) to strip navigation,
ads, sidebars, and other boilerplate. Returns only the article body with title
and byline. Much more token-efficient than WebFetch for article pages.`,
  args: {
    url: tool.schema.string().describe("URL of the article to fetch"),
  },
  async execute(args, context) {
    await context.ask({
      permission: "fetch_article",
      patterns: [args.url],
      always: ["*"],
      metadata: { url: args.url },
    })

    const response = await fetch(args.url, {
      signal: context.abort,
      headers: {
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
      },
    })

    if (!response.ok) {
      throw new Error(`HTTP ${response.status} ${response.statusText}`)
    }

    const html = await response.text()

    const { Readability } = await import("@mozilla/readability")
    const { JSDOM } = await import("jsdom")
    const { default: TurndownService } = await import("turndown")

    const dom = new JSDOM(html, { url: args.url })
    const article = new Readability(dom.window.document).parse()

    if (!article) {
      throw new Error("Readability could not extract article content from this page")
    }

    const turndown = new TurndownService({
      headingStyle: "atx",
      hr: "---",
      bulletListMarker: "-",
      codeBlockStyle: "fenced",
    })
    turndown.remove(["script", "style", "noscript"])
    const markdown = turndown.turndown(article.content)

    const header = [`# ${article.title}`]
    if (article.byline) header.push(`**By:** ${article.byline}`)
    header.push(`**Source:** ${args.url}`)

    return header.join("\n") + "\n\n" + markdown
  },
})
