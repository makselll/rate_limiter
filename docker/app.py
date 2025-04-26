from flask import Flask, jsonify
import os

app = Flask(__name__)

@app.route('/')
def hello():
    hostname = os.environ.get('HOSTNAME', 'unknown')
    return jsonify({
        'message': 'Hello from Python server!',
        'server_id': hostname
    })

if __name__ == '__main__':
    app.run(host='0.0.0.0', port=5000) 