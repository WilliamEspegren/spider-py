import asyncio

from spider_rs import Website

async def main():
    website = Website("https://choosealicense.com", False).with_headers({ "authorization": "myjwttoken"})
    website.crawl()
    print(website.get_links())

asyncio.run(main())