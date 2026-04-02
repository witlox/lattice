FROM python:3.12-slim
ENV STATE_FILE=/tmp/ckpt
COPY checkpoint.py /app/checkpoint.py
ENTRYPOINT ["python", "/app/checkpoint.py"]
