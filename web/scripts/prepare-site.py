"""Prepare direct browser routes and metadata from the Dioxus web bundle."""
from pathlib import Path
import json,re,shutil

root=Path(__file__).resolve().parents[1]
dist=root/'dist'
index=dist/'index.html'
if (dist/'public/index.html').exists() or not index.exists():
    if (dist/'public/index.html').exists():
        candidates=[dist/'public/index.html']
    else:
        candidates=list(dist.rglob('index.html'))
    if len(candidates)!=1:
        raise SystemExit('Dioxus did not produce a single web index.html')
    source=candidates[0].parent
    staging=root/'bundle-staging'
    if staging.exists():
        shutil.rmtree(staging)
    shutil.copytree(source,staging)
    shutil.rmtree(dist)
    staging.rename(dist)
html=index.read_text()
html=re.sub(r'<meta\s+name=[\"\']viewport[\"\'][^>]*>', '', html, flags=re.I)
site='https://courtside-assistant.jass-malak12.chatgpt.site'
metadata='''<meta name="description" content="Courtside is an AI phone assistant. Plan calls, follow live transcripts, listen in, take over, and keep the summary. Built with Rust and Dioxus.">
<meta property="og:type" content="website">
<meta property="og:title" content="Courtside — Your calls, handled.">
<meta property="og:description" content="Give your next phone call to Courtside. Explore the demo or open the web app.">
<meta property="og:url" content="'''+site+'''/">
<meta name="theme-color" content="#07080d">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">'''
html=re.sub(r'<title>.*?</title>','<title>Courtside — Your calls, handled.</title>',html,flags=re.S)
html=html.replace('</head>',metadata+'\n</head>')
index.write_text(html)
for route in ['app','demo']:
    directory=dist/route
    directory.mkdir(exist_ok=True)
    (directory/'index.html').write_text(html)
(dist/'robots.txt').write_text('User-agent: *\nAllow: /\nDisallow: /app/\nSitemap: '+site+'/sitemap.xml\n')
(dist/'sitemap.xml').write_text('<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>'+site+'/</loc></url></urlset>')
print('Prepared Dioxus landing page, /app/, and /demo/ in '+str(dist))
