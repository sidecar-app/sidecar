FROM denoland/deno:alpine AS build

WORKDIR /app

COPY deno.json deno.lock ./
COPY packages/viewer/package.json packages/viewer/
COPY host-app/package.json host-app/
COPY viewer-web/package.json viewer-web/

RUN deno install

COPY packages/viewer/ packages/viewer/
COPY viewer-web/ viewer-web/

ARG VITE_SIGNALING_URL
ENV VITE_SIGNALING_URL=$VITE_SIGNALING_URL

WORKDIR /app/viewer-web
RUN deno task build

FROM nginx:alpine

COPY viewer-web/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /app/viewer-web/dist /usr/share/nginx/html

EXPOSE 80
